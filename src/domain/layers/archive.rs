//! `docker save` parsing: an outer tar holding `manifest.json`, a config
//! blob and one `layer.tar` per layer. Two passes: metadata first (the
//! manifest and config are small), then each layer's inner tar. See
//! dive `image/docker/image_archive.go`.

use crate::domain::layers::{Entry, Kind, Layer, LayerSet, Tree, hash_bytes};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use tar::EntryType;

/// Parse a `docker save` archive into ordered layers.
pub fn parse<R: Read + Seek>(mut reader: R) -> anyhow::Result<LayerSet> {
    // --- Pass 1: the two metadata blobs.
    let manifest = read_named(&mut reader, "manifest.json")?;
    let manifest: Value = serde_json::from_slice(&manifest)?;
    let first = manifest
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow::anyhow!("manifest.json is empty"))?;
    let config_name = first["Config"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("manifest Config is not a string"))?
        .trim_start_matches("./")
        .to_string();
    let layer_paths: Vec<String> = first["Layers"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("manifest Layers is not an array"))?
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_default()
                .trim_start_matches("./")
                .to_string()
        })
        .collect();

    // Each scan is a full pass over the outer tar: `entries_with_seek`
    // skips unread data with relative seeks, so a fresh `Archive` starts
    // wherever the reader currently sits — rewind between passes.
    reader.seek(SeekFrom::Start(0))?;
    let config = read_named(&mut reader, &config_name)?;
    let config: Value = serde_json::from_slice(&config)?;
    let diff_ids: Vec<String> = config["rootfs"]["diff_ids"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("config has no rootfs.diff_ids"))?
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();

    // Rule 2 (`image_archive.go:266`): skip the empty history entries, then
    // pair the rest positionally with the diff_ids.
    let commands: Vec<String> = config["history"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("config has no history"))?
        .iter()
        .filter(|h| !h["empty_layer"].as_bool().unwrap_or(false))
        .filter_map(|h| h["created_by"].as_str().map(str::to_string))
        .collect();
    if commands.len() != diff_ids.len() {
        anyhow::bail!(
            "history/diff_id mismatch: {} non-empty entries, {} diff ids",
            commands.len(),
            diff_ids.len()
        );
    }

    // --- Pass 2: every layer tar, in manifest order.
    let mut layers = Vec::with_capacity(layer_paths.len());
    for (index, path) in layer_paths.iter().enumerate() {
        let (command, id) = commands
            .get(index)
            .cloned()
            .zip(diff_ids.get(index).cloned())
            .ok_or_else(|| anyhow::anyhow!("layer {index} has no history entry"))?;
        reader.seek(SeekFrom::Start(0))?;
        let (size, tree, removed) = parse_layer(&mut reader, path)?;
        layers.push(Layer {
            index,
            id,
            command,
            size,
            removed,
            tree,
        });
    }
    Ok(LayerSet { layers })
}

/// Read one entry of the outer archive into memory.
fn read_named<R: Read + Seek>(reader: &mut R, wanted: &str) -> anyhow::Result<Vec<u8>> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries_with_seek()? {
        let mut entry = entry?;
        let path = entry
            .path()?
            .to_string_lossy()
            .trim_start_matches("./")
            .to_string();
        if path == wanted {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    anyhow::bail!("{wanted} not found in the archive")
}

/// Parse one layer tar (streaming — the outer entry is the reader), apply
/// the whiteout rules, and return the bytes the layer occupies, its tree
/// and the cross-layer removals its whiteouts declare.
fn parse_layer<R: Read + Seek>(
    reader: &mut R,
    wanted: &str,
) -> anyhow::Result<(u64, Tree, Vec<String>)> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries_with_seek()? {
        let entry = entry?;
        let path = entry
            .path()?
            .to_string_lossy()
            .trim_start_matches("./")
            .to_string();
        if path != wanted {
            continue;
        }
        let mut size = 0u64;
        let mut tree = Tree::new();
        let mut removed = Vec::new();
        let mut inner = tar::Archive::new(entry);
        for e in inner.entries()? {
            let mut e = e?;
            size += e.header().size()?;
            let name = e
                .path()?
                .to_string_lossy()
                .trim_start_matches("./")
                .to_string();
            if let Some(dir) = std::path::Path::new(&name)
                .file_name()
                .and_then(|n| n.to_str())
            {
                // Rule 1: the opaque whiteout is never inserted.
                if dir == ".wh..wh..opq" {
                    continue;
                }
                // Rule 1: `.wh.<name>` removes `<name>` in the same directory.
                if let Some(target) = dir.strip_prefix(".wh.") {
                    let parent = std::path::Path::new(&name)
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let victim = format!("/{parent}/{target}");
                    tree.remove_path(&victim);
                    // The marker only bites this layer's own tree (fresh,
                    // independent trees); record it so stacking can apply
                    // the deletion to lower layers.
                    removed.push(victim);
                    continue;
                }
            }
            let head = e.header().clone();
            let kind = match head.entry_type() {
                EntryType::Regular => Kind::File,
                EntryType::Directory => Kind::Dir,
                EntryType::Symlink => Kind::Symlink,
                EntryType::Link => Kind::Hardlink,
                _ => Kind::Other,
            };
            let (hash, payload_size) = if kind == Kind::File {
                let mut buf = Vec::new();
                e.read_to_end(&mut buf)?;
                (hash_bytes(&buf), head.size()?)
            } else {
                (0, 0)
            };
            let link = head
                .link_name()
                .ok()
                .flatten()
                .map(|p| p.to_string_lossy().into_owned());
            tree.add_path(Entry {
                path: format!("/{name}"),
                size: payload_size,
                mode: head.mode()?,
                uid: u32::try_from(head.uid()?)?,
                gid: u32::try_from(head.gid()?)?,
                kind,
                link,
                hash,
            });
        }
        return Ok((size, tree, removed));
    }
    anyhow::bail!("layer {wanted} not found in the archive")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tar::{Builder, EntryType, Header};

    fn header(path: &str, size: u64) -> Header {
        let mut h = Header::new_gnu();
        h.set_path(path).unwrap();
        h.set_size(size);
        h.set_mode(0o644);
        h.set_uid(0);
        h.set_gid(0);
        h.set_entry_type(EntryType::Regular);
        h.set_cksum();
        h
    }

    /// One layer's inner tar: `(path, contents)` pairs.
    fn layer_tar(files: &[(&str, &str)]) -> Vec<u8> {
        let mut b = Builder::new(Vec::new());
        for (path, contents) in files {
            b.append_data(
                &mut header(path, contents.len() as u64),
                *path,
                contents.as_bytes(),
            )
            .unwrap();
        }
        b.into_inner().unwrap()
    }

    /// A synthetic `docker save`: one `layer.tar` per entry of `layers`,
    /// a config blob, and `manifest.json`. `history` carries the
    /// `(created_by, empty_layer)` pairs, `diff_ids` the rootfs ids.
    fn save_tar(
        layers: &[Vec<(&str, &str)>],
        history: &[(&str, bool)],
        diff_ids: &[&str],
    ) -> Vec<u8> {
        let mut b = Builder::new(Vec::new());
        let layer_paths: Vec<String> = layers
            .iter()
            .enumerate()
            .map(|(i, _)| format!("layers/{i}/layer.tar"))
            .collect();
        for (i, files) in layers.iter().enumerate() {
            let data = layer_tar(files);
            b.append_data(
                &mut header(&layer_paths[i], data.len() as u64),
                &layer_paths[i],
                &data[..],
            )
            .unwrap();
        }
        let config = format!(
            r#"{{"rootfs":{{"diff_ids":[{}]}},"history":[{}]}}"#,
            diff_ids
                .iter()
                .map(|d| format!(r#""{d}""#))
                .collect::<Vec<_>>()
                .join(","),
            history
                .iter()
                .map(|(cmd, empty)| format!(r#"{{"created_by":"{cmd}","empty_layer":{empty}}}"#))
                .collect::<Vec<_>>()
                .join(","),
        );
        b.append_data(
            &mut header("config.json", config.len() as u64),
            "config.json",
            config.as_bytes(),
        )
        .unwrap();
        // The manifest declares its blobs with a "./" prefix (as real
        // saves may); `parse` must trim it before matching entry paths.
        let manifest = format!(
            r#"[{{"Config":"./config.json","RepoTags":["t:latest"],"Layers":[{}]}}]"#,
            layer_paths
                .iter()
                .map(|p| format!(r#""./{p}""#))
                .collect::<Vec<_>>()
                .join(","),
        );
        b.append_data(
            &mut header("manifest.json", manifest.len() as u64),
            "manifest.json",
            manifest.as_bytes(),
        )
        .unwrap();
        b.into_inner().unwrap()
    }

    fn two_layer(save: Vec<u8>) -> LayerSet {
        parse(Cursor::new(save)).unwrap()
    }

    #[test]
    fn empty_history_entries_are_skipped_and_the_rest_pair_with_diff_ids() {
        let set = two_layer(save_tar(
            &[vec![("bin/busybox", "v1")], vec![("bin/busybox", "v2")]],
            &[
                ("FROM alpine", true),
                ("RUN apk add", false),
                ("COPY app", false),
            ],
            &["sha256:aaa", "sha256:bbb"],
        ));
        assert_eq!(set.layers.len(), 2, "the empty layer is skipped");
        assert_eq!(set.layers[0].id, "sha256:aaa");
        assert_eq!(set.layers[0].command, "RUN apk add");
        assert_eq!(set.layers[1].id, "sha256:bbb");
        assert_eq!(set.layers[1].command, "COPY app");
    }

    #[test]
    fn entries_land_in_the_tree_with_their_sizes() {
        let set = two_layer(save_tar(
            &[vec![("etc/hosts", "127.0.0.1")]],
            &[("COPY etc", false)],
            &["sha256:aaa"],
        ));
        let id = set.layers[0].tree.get("/etc/hosts").expect("entry");
        let entry = set.layers[0].tree.node(id).entry.as_ref().unwrap();
        assert_eq!(entry.size, "127.0.0.1".len() as u64);
        assert_eq!(set.layers[0].size, "127.0.0.1".len() as u64);
        assert_eq!(entry.kind, Kind::File);
        assert_eq!(entry.hash, hash_bytes(b"127.0.0.1"));
    }

    #[test]
    fn a_dot_slash_prefix_is_cleaned() {
        let set = two_layer(save_tar(
            &[vec![("etc/passwd", "x")]],
            &[("COPY", false)],
            &["sha256:aaa"],
        ));
        assert!(set.layers[0].tree.get("/etc/passwd").is_some());
        assert!(set.layers[0].tree.get("etc/passwd").is_some());
    }

    #[test]
    fn identical_contents_hash_equal_and_changed_contents_do_not() {
        let set = two_layer(save_tar(
            &[vec![("a.txt", "same")], vec![("a.txt", "changed")]],
            &[("one", false), ("two", false)],
            &["sha256:aaa", "sha256:bbb"],
        ));
        let h = |set: &LayerSet, i: usize| {
            let t = &set.layers[i].tree;
            t.node(t.get("/a.txt").unwrap())
                .entry
                .as_ref()
                .unwrap()
                .hash
        };
        let set2 = two_layer(save_tar(
            &[vec![("a.txt", "same")], vec![("a.txt", "same")]],
            &[("one", false), ("two", false)],
            &["sha256:aaa", "sha256:bbb"],
        ));
        assert_eq!(h(&set2, 0), h(&set2, 1), "identical contents hash equal");
        assert_ne!(h(&set, 0), h(&set, 1), "one byte changed the hash");
    }

    #[test]
    fn a_whiteout_removes_the_previous_layer_path() {
        let set = two_layer(save_tar(
            &[
                vec![("etc/hosts", "x"), ("etc/passwd", "y")],
                vec![("etc/hosts", "z"), ("etc/.wh.hosts", "")],
            ],
            &[("one", false), ("two", false)],
            &["sha256:aaa", "sha256:bbb"],
        ));
        assert!(set.layers[0].tree.get("/etc/hosts").is_some());
        assert!(
            set.layers[1].tree.get("/etc/hosts").is_none(),
            "whiteout target gone"
        );
        assert!(
            set.layers[1].tree.get("/etc/.wh.hosts").is_none(),
            "the marker is never inserted"
        );
        assert!(
            set.layers[1].removed == vec!["/etc/hosts".to_string()],
            "the victim is recorded for cross-layer stacking: {:?}",
            set.layers[1].removed
        );
        assert!(set.layers[1].tree.get("/etc/passwd").is_none());
    }

    #[test]
    fn an_opaque_whiteout_is_dropped_and_never_inserted() {
        let set = two_layer(save_tar(
            &[vec![("etc/hosts", "x")], vec![("etc/.wh..wh..opq", "")]],
            &[("one", false), ("two", false)],
            &["sha256:aaa", "sha256:bbb"],
        ));
        let names: Vec<String> = set.layers[1]
            .tree
            .dfs()
            .into_iter()
            .map(|id| set.layers[1].tree.node(id).name.clone())
            .collect();
        assert!(!names.iter().any(|n| n.contains(".wh")), "{names:?}");
        assert!(set.layers[1].tree.get("/etc/hosts").is_none());
        assert!(
            set.layers[1].removed.is_empty(),
            "opaque whiteouts record nothing"
        );
    }

    #[test]
    fn a_history_and_diff_id_mismatch_is_an_error() {
        let save = save_tar(
            &[vec![("a", "x")]],
            &[("one", false), ("two", false)],
            &["sha256:aaa"],
        );
        assert!(parse(Cursor::new(save)).is_err());
    }

    #[test]
    fn a_missing_manifest_is_an_error() {
        let mut b = Builder::new(Vec::new());
        b.append_data(
            &mut header("layers/x/layer.tar", 0),
            "layers/x/layer.tar",
            &[][..],
        )
        .unwrap();
        let bytes = b.into_inner().unwrap();
        assert!(parse(Cursor::new(bytes)).is_err());
    }
}
