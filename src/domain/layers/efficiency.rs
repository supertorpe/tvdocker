//! Efficiency and potential wasted space over LEAF paths (dive
//! `filetree/efficiency.go:38-131`, `image/analysis.go:20-47`). See the
//! design spec §5.2 rule 6 for the exact formula.

use crate::domain::layers::{InefficientPath, Layer, NodeId, Tree, stack};
use std::collections::HashMap;

/// What one path has been seen doing across the layers: its cumulative
/// size, its SMALLEST observed size (dive scores on the true minimum, not
/// the mean of the copies), and how many layers touched it.
struct Seen {
    total: u64,
    min: u64,
    count: usize,
}

fn observe(seen: &mut HashMap<String, Seen>, path: String, size: u64) {
    let slot = seen.entry(path).or_insert(Seen {
        total: 0,
        min: u64::MAX,
        count: 0,
    });
    slot.total += size;
    slot.min = slot.min.min(size);
    slot.count += 1;
}

/// The summed sizes of `id`'s whole subtree (rule 6 sizes a removed
/// directory by what it held; directory entries carry no size of their
/// own, so this is effectively the leaf sum).
fn subtree_size(tree: &Tree, id: NodeId) -> u64 {
    let mut total = 0u64;
    let mut todo = vec![id];
    while let Some(id) = todo.pop() {
        if let Some(entry) = tree.node(id).entry.as_ref() {
            total += entry.size;
        }
        todo.extend(tree.children(id));
    }
    total
}

/// `(efficiency, wasted_bytes, inefficient_paths)`.
pub fn analyze(layers: &[Layer]) -> (f64, u64, Vec<InefficientPath>) {
    // path -> what every layer that touched it contributed.
    let mut seen: HashMap<String, Seen> = HashMap::new();
    for (i, layer) in layers.iter().enumerate() {
        // Rule 6: a whiteout contributes the size of what it removed,
        // measured against the PRIOR layers only (this layer's own tree is
        // independent of the stacking order).
        if !layer.removed.is_empty() {
            let prior = stack(&layers[..i]);
            for victim in &layer.removed {
                let Some(id) = prior.get(victim) else {
                    continue; // nothing lower ever had it: contribute nothing
                };
                // A removed file contributes 0 — dive's own behaviour: that
                // 0 is what pulls the path's minimum down for deleted files.
                let size = if prior.is_dir(id) {
                    subtree_size(&prior, id)
                } else {
                    0
                };
                observe(&mut seen, prior.path_of(id), size);
            }
        }
        // dive's visitEvaluator is `IsLeaf`, not `kind == File`: symlinks
        // and empty directories count too.
        for id in layer.tree.dfs() {
            let Some(entry) = layer.tree.node(id).entry.as_ref() else {
                continue; // the root and synthesised intermediates hold no entry
            };
            if !layer.tree.children(id).is_empty() {
                continue;
            }
            observe(&mut seen, entry.path.clone(), entry.size);
        }
    }

    let mut total = 0u64;
    let mut minimum = 0u64;
    for slot in seen.values() {
        total += slot.total;
        minimum += slot.min;
    }
    let efficiency = if total == 0 {
        1.0
    } else {
        minimum as f64 / total as f64
    };

    // Wasted = the cumulative size of every path seen in >= 2 layers,
    // ascending (the *potential*, per dive's own labelling).
    let mut inefficient: Vec<InefficientPath> = seen
        .into_iter()
        .filter(|(_, slot)| slot.count >= 2)
        .map(|(path, slot)| InefficientPath {
            count: slot.count,
            wasted: slot.total,
            path,
        })
        .collect();
    inefficient.sort_by(|a, b| a.wasted.cmp(&b.wasted).then_with(|| a.path.cmp(&b.path)));

    let wasted_bytes = inefficient.iter().map(|p| p.wasted).sum();
    (efficiency, wasted_bytes, inefficient)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::layers::{Entry, Kind, Tree, hash_bytes};

    fn layer(index: usize, files: &[(&str, u64)]) -> Layer {
        let mut tree = Tree::new();
        for (path, size) in files {
            tree.add_path(Entry {
                path: path.to_string(),
                size: *size,
                mode: 0o644,
                uid: 0,
                gid: 0,
                kind: Kind::File,
                link: None,
                hash: hash_bytes(path.as_bytes()),
            });
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

    #[test]
    fn a_file_written_twice_scores_a_half() {
        let layers = vec![layer(0, &[("/a", 100)]), layer(1, &[("/a", 100)])];
        let (eff, wasted, inefficient) = analyze(&layers);
        assert_eq!(eff, 0.5, "min 100 / total 200");
        assert_eq!(wasted, 200, "the path's cumulative size");
        assert_eq!(inefficient.len(), 1);
        assert_eq!(inefficient[0].count, 2);
        assert_eq!(inefficient[0].path, "/a");
    }

    #[test]
    fn disjoint_layers_are_fully_efficient() {
        let layers = vec![layer(0, &[("/a", 100)]), layer(1, &[("/b", 50)])];
        let (eff, wasted, inefficient) = analyze(&layers);
        assert_eq!(eff, 1.0);
        assert_eq!(wasted, 0);
        assert!(inefficient.is_empty());
    }

    #[test]
    fn an_empty_image_scores_one_not_nan() {
        let (eff, wasted, inefficient) = analyze(&[]);
        assert_eq!(eff, 1.0, "no division by zero");
        assert_eq!(wasted, 0);
        assert!(inefficient.is_empty());
    }

    #[test]
    fn directories_are_not_counted_as_leaves() {
        let layers = vec![layer(0, &[("/etc/hosts", 100), ("/etc/passwd", 100)])];
        let (eff, wasted, _) = analyze(&layers);
        assert_eq!(eff, 1.0, "two distinct leaves, each seen once");
        assert_eq!(wasted, 0);
    }

    #[test]
    fn the_inefficient_list_is_ascending_by_waste() {
        let layers = vec![
            layer(0, &[("/big", 900), ("/small", 10)]),
            layer(1, &[("/big", 900), ("/small", 10)]),
        ];
        let (_, _, inefficient) = analyze(&layers);
        assert_eq!(inefficient.len(), 2);
        assert!(
            inefficient[0].wasted <= inefficient[1].wasted,
            "{inefficient:?}"
        );
    }

    /// Ruling 2: dive scores each path on its TRUE minimum across sightings,
    /// not the mean of them — 100/300, not 150/300.
    #[test]
    fn a_varying_size_file_scores_on_its_true_minimum() {
        let layers = vec![layer(0, &[("/a", 100)]), layer(1, &[("/a", 200)])];
        let (eff, wasted, inefficient) = analyze(&layers);
        assert!((eff - 100.0 / 300.0).abs() < 1e-12, "{eff} is min/total");
        assert_eq!(wasted, 300, "the path's cumulative size");
        assert_eq!(inefficient.len(), 1);
        assert_eq!(inefficient[0].count, 2);
    }

    /// Ruling 4: a removed FILE contributes 0, which pulls the path's
    /// minimum to 0 — dive's way of discounting deleted files.
    #[test]
    fn a_file_whiteout_pulls_the_minimum_to_zero() {
        let mut del = layer(1, &[]);
        del.removed = vec!["/a".to_string()];
        let layers = vec![layer(0, &[("/a", 100)]), del];
        let (eff, wasted, inefficient) = analyze(&layers);
        assert_eq!(eff, 0.0, "min 0 / total 100");
        assert_eq!(wasted, 100);
        assert_eq!(
            inefficient,
            vec![InefficientPath {
                count: 2,
                wasted: 100,
                path: "/a".to_string(),
            }]
        );
    }

    /// Ruling 4: a whiteout of a path no lower layer ever had sizes nothing.
    #[test]
    fn a_whiteout_of_an_absent_path_contributes_nothing() {
        let mut del = layer(1, &[]);
        del.removed = vec!["/ghost".to_string()];
        let layers = vec![layer(0, &[("/a", 100)]), del];
        let (eff, wasted, _) = analyze(&layers);
        assert_eq!(eff, 1.0, "/a is the only sighting: min 100 / total 100");
        assert_eq!(wasted, 0);
    }

    /// Ruling 4: a removed DIRECTORY contributes the summed sizes of its
    /// subtree. `/dir` is seen three ways: whiteout of 100 (layers 0..1 hold
    /// 60 + 40 under it), then a 50-byte file re-created at the same path in
    /// layer 2 — min 50 / total 150, so the whiteout's 100 is observable.
    #[test]
    fn a_directory_whiteout_contributes_the_subtree_sum() {
        let mut del = layer(1, &[]);
        del.removed = vec!["/dir".to_string()];
        let layers = vec![
            layer(0, &[("/dir/a", 60), ("/dir/b", 40)]),
            del,
            layer(2, &[("/dir", 50)]),
        ];
        let (eff, wasted, inefficient) = analyze(&layers);
        assert!(
            (eff - 150.0 / 250.0).abs() < 1e-12,
            "{eff} = (50+60+40)/total"
        );
        assert_eq!(wasted, 150, "50 re-created + 100 whiteout subtree");
        assert_eq!(
            inefficient,
            vec![InefficientPath {
                count: 2,
                wasted: 150,
                path: "/dir".to_string(),
            }]
        );
    }

    /// Ruling 3: dive visits leaves (`IsLeaf`), not `kind == File` — a
    /// symlink written twice is inefficient too.
    #[test]
    fn a_symlink_written_twice_is_counted_as_a_leaf() {
        let symlink_layer = |index: usize| {
            let mut tree = Tree::new();
            tree.add_path(Entry {
                path: "/link".to_string(),
                size: 0,
                mode: 0o777,
                uid: 0,
                gid: 0,
                kind: Kind::Symlink,
                link: Some("/a".to_string()),
                hash: 0,
            });
            Layer {
                index,
                id: format!("sha256:{index}"),
                command: format!("layer {index}"),
                size: 0,
                removed: Vec::new(),
                tree,
            }
        };
        let layers = vec![symlink_layer(0), symlink_layer(1)];
        let (_, wasted, inefficient) = analyze(&layers);
        assert_eq!(wasted, 0, "symlinks carry no payload size");
        assert_eq!(
            inefficient,
            vec![InefficientPath {
                count: 2,
                wasted: 0,
                path: "/link".to_string(),
            }]
        );
    }
}
