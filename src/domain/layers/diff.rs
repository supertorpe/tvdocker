//! Layer comparison: mark one tree against another, roll directories up,
//! and stack layers into a baseline. Ported from dive's
//! `filetree/{diff,comparer,file_node}.go`; the rules are in the design
//! spec §5.2.

use crate::domain::layers::{DiffType, Layer, Tree};
use std::collections::HashMap;

/// Which comparison a window is showing (rule 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compare {
    /// Layer N against the stack of 0..N-1.
    Single,
    /// Layer 0 against the stack of 0..N.
    Aggregated,
}

/// Mark `top`'s nodes against `bottom`, then cascade the removals.
pub fn compare(bottom: &Tree, top: &Tree) -> Tree {
    let mut out = top.clone();
    // Rule 3 (`file_info.go:111`): equal only when type, hash, mode, uid,
    // gid all match — a chmod is a modification.
    for id in out.dfs() {
        let theirs = out.node(id).entry.clone();
        let ours = theirs.as_ref().and_then(|e| {
            bottom
                .get(&e.path)
                .and_then(|bid| bottom.node(bid).entry.clone())
        });
        let diff = match (&theirs, &ours) {
            (None, _) => DiffType::Unmodified, // a directory: rolled up below
            (Some(_), None) => DiffType::Added,
            (Some(t), Some(b)) if t.same_content(b) => DiffType::Unmodified,
            (Some(_), Some(_)) => DiffType::Modified,
        };
        out.node_mut(id).diff = diff;
    }
    // Rule 4a: everything in `bottom` but not in `top` is removed.
    for id in bottom.dfs() {
        let path = bottom.path_of(id);
        if out.get(&path).is_none()
            && let Some(entry) = bottom.node(id).entry.clone()
        {
            let nid = out.add_path(entry);
            out.node_mut(nid).diff = DiffType::Removed;
        }
    }
    roll_up(&mut out);
    out
}

/// Rule 4: a directory is `Modified` unless every child agrees; an all
/// removed children make the directory `Removed`, which cascades.
fn roll_up(tree: &mut Tree) {
    // Reverse DFS so children are rolled up before their parent.
    for id in tree.dfs().into_iter().rev() {
        let children = tree.children(id);
        if children.is_empty() {
            continue;
        }
        let mut diffs: Vec<DiffType> = children.iter().map(|c| tree.node(*c).diff).collect();
        let merged = if diffs.iter().all(|d| *d == DiffType::Removed) {
            DiffType::Removed
        } else if diffs.windows(2).all(|w| w[0] == w[1]) {
            diffs.remove(0)
        } else {
            DiffType::Modified
        };
        tree.node_mut(id).diff = merged;
    }
}

/// Flatten `layers` into one tree, later layers overwriting earlier ones.
/// Each layer's recorded whiteout removals are applied BEFORE its own
/// additions, so a layer that deletes and then re-creates a path wins
/// (overlayfs ordering); `remove_path` drops the whole subtree.
pub fn stack(layers: &[Layer]) -> Tree {
    let mut out = Tree::new();
    for layer in layers {
        for victim in &layer.removed {
            out.remove_path(victim);
        }
        for id in layer.tree.dfs() {
            if let Some(entry) = layer.tree.node(id).entry.clone() {
                out.add_path(entry);
            }
        }
    }
    out
}

/// The compare results, cached by `(bottom_start, bottom_stop, top_start,
/// top_stop)` — the dive comparer key.
#[derive(Default)]
pub struct Comparer {
    results: HashMap<(usize, usize, usize, usize), Tree>,
}

impl Comparer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The tree for `layers`, computing each key at most once (rule 5,
    /// `comparer.go:86-146`).
    pub fn tree<'a>(&'a mut self, layers: &[Layer], cmp: Compare, selected: usize) -> &'a Tree {
        // The dive comparer key, adapted: `compare` replaces dive's
        // in-place `CompareAndMark`, so the top range must be the whole
        // stack through `selected` (the filesystem as of that layer) —
        // marking layer N's tar alone would read every baseline file the
        // tar does not re-list as removed. `selected == 0` is always the
        // base: it compares layer 0 against itself, which is all
        // `Unmodified`.
        let key = match cmp {
            Compare::Single => (0usize, selected.saturating_sub(1), 0, selected),
            Compare::Aggregated if selected == 0 => (0, 0, 0, 0),
            Compare::Aggregated => (0, 0, 0, selected),
        };
        self.results.entry(key).or_insert_with(|| {
            let (bs, bt, ts, tt) = key;
            let bottom = stack(&layers[bs..=bt]);
            let top = stack(&layers[ts..=tt]);
            compare(&bottom, &top)
        });
        &self.results[&key]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::layers::{Entry, Kind, hash_bytes};

    /// A one-layer fixture: each `(path, contents)` is hashed like the
    /// archive parser would.
    fn layer(index: usize, files: &[(&str, &str)]) -> Layer {
        let mut tree = Tree::new();
        for (path, contents) in files {
            let mut entry = Entry {
                path: path.to_string(),
                size: contents.len() as u64,
                mode: 0o644,
                uid: 0,
                gid: 0,
                kind: Kind::File,
                link: None,
                hash: hash_bytes(contents.as_bytes()),
            };
            if path.ends_with('/') {
                entry.kind = Kind::Dir;
                entry.hash = 0;
            }
            tree.add_path(entry);
        }
        Layer {
            index,
            id: format!("sha256:{index}"),
            command: format!("layer {index}"),
            size: 0,
            removed: Vec::new(),
            tree,
        }
    }

    fn diff_of(t: &Tree, path: &str) -> DiffType {
        t.node(t.get(path).expect("path")).diff
    }

    #[test]
    fn identical_entries_are_unmodified() {
        let a = layer(0, &[("/etc/hosts", "same")]);
        let b = layer(1, &[("/etc/hosts", "same")]);
        assert_eq!(
            diff_of(&compare(&a.tree, &b.tree), "/etc/hosts"),
            DiffType::Unmodified
        );
    }

    #[test]
    fn a_chmod_alone_is_a_modification() {
        let bottom = layer(0, &[("/bin/sh", "sh")]);
        // Brief erratum: the mutation below needs `mut` to compile.
        let mut top = layer(1, &[("/bin/sh", "sh")]);
        top.tree
            .node_mut(top.tree.get("/bin/sh").unwrap())
            .entry
            .as_mut()
            .unwrap()
            .mode = 0o755;
        assert_eq!(
            diff_of(&compare(&bottom.tree, &top.tree), "/bin/sh"),
            DiffType::Modified
        );
    }

    #[test]
    fn a_changed_file_is_modified() {
        let bottom = layer(0, &[("/a", "v1")]);
        let top = layer(1, &[("/a", "v2")]);
        assert_eq!(
            diff_of(&compare(&bottom.tree, &top.tree), "/a"),
            DiffType::Modified
        );
    }

    #[test]
    fn a_new_file_is_added_and_a_missing_one_is_removed() {
        let bottom = layer(0, &[("/a", "v1"), ("/b", "v1")]);
        let top = layer(1, &[("/a", "v1"), ("/c", "v1")]);
        let got = compare(&bottom.tree, &top.tree);
        assert_eq!(diff_of(&got, "/c"), DiffType::Added);
        assert_eq!(diff_of(&got, "/b"), DiffType::Removed);
        assert_eq!(diff_of(&got, "/a"), DiffType::Unmodified);
    }

    #[test]
    fn a_directory_is_modified_when_any_child_changed() {
        let bottom = layer(0, &[("/etc/hosts", "v1")]);
        let top = layer(1, &[("/etc/hosts", "v2")]);
        let got = compare(&bottom.tree, &top.tree);
        assert_eq!(diff_of(&got, "/etc"), DiffType::Modified);
        assert_eq!(diff_of(&got, "/"), DiffType::Modified);
    }

    #[test]
    fn removing_a_directory_cascades_to_every_descendant() {
        let bottom = layer(0, &[("/etc/sub/a", "v1"), ("/etc/sub/b", "v1")]);
        let top = layer(1, &[]);
        let got = compare(&bottom.tree, &top.tree);
        assert_eq!(diff_of(&got, "/etc/sub"), DiffType::Removed);
        assert_eq!(diff_of(&got, "/etc/sub/a"), DiffType::Removed);
        assert_eq!(diff_of(&got, "/etc/sub/b"), DiffType::Removed);
    }

    #[test]
    fn stack_honors_layer_removals() {
        let mut del = layer(1, &[]);
        del.removed = vec!["/etc/hosts".to_string()];
        let layers = vec![layer(0, &[("/etc/hosts", "v1")]), del];
        assert!(stack(&layers).get("/etc/hosts").is_none());
    }

    #[test]
    fn a_deleted_path_shows_as_removed_in_the_comparer() {
        let mut del = layer(1, &[]);
        del.removed = vec!["/etc/hosts".to_string()];
        let layers = vec![
            layer(0, &[("/etc/hosts", "v1"), ("/etc/passwd", "v1")]),
            del,
        ];
        let mut c = Comparer::new();
        let tree = c.tree(&layers, Compare::Single, 1);
        assert_eq!(diff_of(tree, "/etc/hosts"), DiffType::Removed);
        assert_eq!(diff_of(tree, "/etc/passwd"), DiffType::Unmodified);
    }

    #[test]
    fn removals_apply_before_additions_in_the_same_layer() {
        let mut recreate = layer(1, &[("/etc/hosts", "v1")]);
        recreate.removed = vec!["/etc/hosts".to_string()];
        let layers = vec![layer(0, &[("/etc/hosts", "v0")]), recreate];
        let flat = stack(&layers);
        let id = flat.get("/etc/hosts").expect("re-created after removal");
        assert_eq!(
            flat.node(id).entry.as_ref().unwrap().hash,
            hash_bytes(b"v1"),
            "overlayfs order: the layer's own write wins over its whiteout"
        );
    }

    #[test]
    fn stack_lets_the_later_layer_win() {
        let layers = vec![layer(0, &[("/a", "v1")]), layer(1, &[("/a", "v2")])];
        let flat = stack(&layers);
        let id = flat.get("/a").unwrap();
        assert_eq!(
            flat.node(id).entry.as_ref().unwrap().hash,
            hash_bytes(b"v2"),
            "the later layer's content wins"
        );
        assert_eq!(flat.get("/a").unwrap().to_string(), id.to_string());
    }

    #[test]
    fn the_base_layer_alone_is_all_unmodified() {
        let layers = vec![layer(0, &[("/a", "v1")]), layer(1, &[("/b", "v1")])];
        let mut c = Comparer::new();
        let tree = c.tree(&layers, Compare::Single, 0);
        assert_eq!(diff_of(tree, "/a"), DiffType::Unmodified);
    }

    #[test]
    fn single_mode_marks_the_selected_layer_only() {
        let layers = vec![
            layer(0, &[("/a", "v1")]),
            layer(1, &[("/b", "v1")]),
            layer(2, &[("/c", "v1")]),
        ];
        let mut c = Comparer::new();
        let tree = c.tree(&layers, Compare::Single, 2);
        assert_eq!(diff_of(tree, "/c"), DiffType::Added);
        assert_eq!(diff_of(tree, "/a"), DiffType::Unmodified, "in the baseline");
        assert_eq!(diff_of(tree, "/b"), DiffType::Unmodified);
    }

    #[test]
    fn a_repeated_key_is_computed_once() {
        let layers = vec![layer(0, &[("/a", "v1")]), layer(1, &[("/a", "v2")])];
        let mut c = Comparer::new();
        let first = c.tree(&layers, Compare::Single, 1) as *const Tree;
        let second = c.tree(&layers, Compare::Single, 1) as *const Tree;
        assert_eq!(first, second, "same allocation, no recomputation");
    }
}
