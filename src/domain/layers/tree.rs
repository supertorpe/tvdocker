//! The file-tree arena: layers store their filesystem as indices into one
//! `Vec<Node>` instead of pointer-linked boxes. Dive's algorithms port
//! cleanly onto this; a literal port of its pointer tree does not.
use super::{DiffType, Entry, Kind};

/// Index into `Tree::nodes`. `0` is always the root.
pub type NodeId = usize;

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    /// Children are PREPENDED, so `children()` returns newest-first and the
    /// pre-order DFS visits them oldest-first (insertion order).
    pub next_sibling: Option<NodeId>,
    /// One path component ("" on the root).
    pub name: String,
    /// Filled by `diff::compare`; always `Unmodified` while building.
    pub diff: DiffType,
    /// `None` on a synthesised intermediate directory.
    pub entry: Option<Entry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tree {
    nodes: Vec<Node>,
    root: NodeId,
}

// Hand-written because the derived `Default` would build an empty arena
// (`nodes: []`, `root: 0`) that violates the "0 is always the root"
// invariant and panics on first use.
impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

impl Tree {
    pub fn new() -> Self {
        Self {
            nodes: vec![Node {
                parent: None,
                first_child: None,
                next_sibling: None,
                name: String::new(),
                diff: DiffType::Unmodified,
                entry: None,
            }],
            root: 0,
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Walk `parent`'s children for `name`.
    fn find_child(&self, parent: NodeId, name: &str) -> Option<NodeId> {
        let mut cur = self.nodes[parent].first_child;
        while let Some(id) = cur {
            if self.nodes[id].name == name {
                return Some(id);
            }
            cur = self.nodes[id].next_sibling;
        }
        None
    }

    /// The child of `parent` named `name`, creating it (as an empty
    /// intermediate directory) when absent. Prepending keeps the DFS in
    /// insertion order.
    fn make_child(&mut self, parent: NodeId, name: &str) -> NodeId {
        if let Some(id) = self.find_child(parent, name) {
            return id;
        }
        let id = self.nodes.len();
        self.nodes.push(Node {
            parent: Some(parent),
            first_child: None,
            next_sibling: self.nodes[parent].first_child,
            name: name.to_string(),
            diff: DiffType::Unmodified,
            entry: None,
        });
        self.nodes[parent].first_child = Some(id);
        id
    }

    fn parts(path: &str) -> Vec<&str> {
        path.split('/').filter(|p| !p.is_empty()).collect()
    }

    /// Insert `entry`, creating any missing intermediate directories.
    /// Returns the leaf id (or the root for `"/"`).
    pub fn add_path(&mut self, entry: Entry) -> NodeId {
        let parts = Self::parts(&entry.path);
        let mut cur = self.root;
        for part in &parts[..parts.len().saturating_sub(1)] {
            cur = self.make_child(cur, part);
        }
        match parts.last() {
            Some(leaf) => {
                let id = self.make_child(cur, leaf);
                self.nodes[id].entry = Some(entry);
                id
            }
            None => cur,
        }
    }

    pub fn get(&self, path: &str) -> Option<NodeId> {
        let mut cur = self.root;
        for part in Self::parts(path) {
            cur = self.find_child(cur, part)?;
        }
        Some(cur)
    }

    /// Detach the node at `path` and its whole subtree. `false` when absent
    /// (idempotent — whiteouts may repeat).
    ///
    /// The detached nodes stay in the arena, so a stale `NodeId` still
    /// resolves through `node()`/`path_of()` and returns plausible
    /// pre-removal data instead of failing. Ids do NOT stay live across
    /// removals: re-resolve paths with `get()` instead of holding ids.
    pub fn remove_path(&mut self, path: &str) -> bool {
        let Some(id) = self.get(path) else {
            return false;
        };
        if id == self.root {
            return false;
        }
        let parent = self.nodes[id].parent.expect("non-root has a parent");
        let mut cur = self.nodes[parent].first_child;
        let mut prev: Option<NodeId> = None;
        while let Some(cid) = cur {
            if cid == id {
                match prev {
                    Some(p) => self.nodes[p].next_sibling = self.nodes[cid].next_sibling,
                    None => self.nodes[parent].first_child = self.nodes[cid].next_sibling,
                }
                return true;
            }
            prev = Some(cid);
            cur = self.nodes[cid].next_sibling;
        }
        false
    }

    /// A node by id. Ids do not stay live across `remove_path`: a detached
    /// node still resolves and returns stale pre-removal data, so re-resolve
    /// with `get()` rather than holding ids across removals.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id]
    }

    /// A node's children, newest first.
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cur = self.nodes[id].first_child;
        while let Some(cid) = cur {
            out.push(cid);
            cur = self.nodes[cid].next_sibling;
        }
        out
    }

    /// Pre-order DFS; siblings in insertion order.
    pub fn dfs(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            out.push(id);
            for child in self.children(id) {
                stack.push(child);
            }
        }
        out
    }

    /// The absolute path of `id` (`"/"` for the root).
    pub fn path_of(&self, id: NodeId) -> String {
        let mut names = Vec::new();
        let mut cur = Some(id);
        while let Some(id) = cur {
            names.push(self.nodes[id].name.as_str());
            cur = self.nodes[id].parent;
        }
        names.reverse();
        format!(
            "/{}",
            names
                .iter()
                .filter(|n| !n.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("/")
        )
    }

    /// Every node's size the way dive shows it (`FileNode.GetSize`): a
    /// file's own payload size, a directory's rolled-up subtree sum that
    /// SKIPS removed children — unless the directory itself is removed,
    /// which then accumulates everything it used to hold. One post-order
    /// pass (reverse DFS: children before parents), indexed by
    /// [`NodeId`], so a caller renders a whole tree from one call.
    pub fn subtree_sizes(&self) -> Vec<u64> {
        let mut sizes = vec![0u64; self.nodes.len()];
        for id in self.dfs().into_iter().rev() {
            let own_removed = self.nodes[id].diff == DiffType::Removed;
            let mut total = self.nodes[id].entry.as_ref().map_or(0, |e| e.size);
            for child in self.children(id) {
                if own_removed || self.nodes[child].diff != DiffType::Removed {
                    total += sizes[child];
                }
            }
            sizes[id] = total;
        }
        sizes
    }

    /// A node without an entry is a directory.
    pub fn is_dir(&self, id: NodeId) -> bool {
        self.nodes[id]
            .entry
            .as_ref()
            .is_none_or(|e| e.kind == Kind::Dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::layers::{Entry, Kind};

    fn sized(path: &str, size: u64) -> Entry {
        Entry {
            path: path.to_string(),
            size,
            mode: 0o644,
            uid: 0,
            gid: 0,
            kind: Kind::File,
            link: None,
            hash: 7,
        }
    }

    fn file(path: &str) -> Entry {
        let mut e = sized(path, 10);
        e.path = path.to_string();
        e
    }

    fn paths(t: &Tree) -> Vec<String> {
        t.dfs().into_iter().map(|id| t.path_of(id)).collect()
    }

    #[test]
    fn add_path_builds_every_intermediate_directory() {
        let mut t = Tree::new();
        t.add_path(file("/a/b/c"));
        assert_eq!(paths(&t), ["/", "/a", "/a/b", "/a/b/c"]);
        assert!(t.is_dir(t.get("/a").unwrap()));
        assert!(!t.is_dir(t.get("/a/b/c").unwrap()));
    }

    #[test]
    fn add_path_reuses_directories_and_keeps_insertion_order() {
        let mut t = Tree::new();
        t.add_path(file("/a/b"));
        t.add_path(file("/a/c"));
        assert_eq!(paths(&t), ["/", "/a", "/a/b", "/a/c"]);
    }

    #[test]
    fn add_path_tolerates_a_missing_leading_slash() {
        let mut t = Tree::new();
        t.add_path(file("etc/passwd")); // tar paths arrive like this
        assert_eq!(paths(&t), ["/", "/etc", "/etc/passwd"]);
        assert!(t.get("/etc/passwd").is_some());
    }

    #[test]
    fn get_misses_what_is_not_there() {
        let mut t = Tree::new();
        t.add_path(file("/etc/passwd"));
        assert!(t.get("/etc/shadow").is_none());
        assert!(t.get("/nope").is_none());
    }

    #[test]
    fn remove_path_detaches_the_leaf() {
        let mut t = Tree::new();
        t.add_path(file("/etc/passwd"));
        t.add_path(file("/etc/shadow"));
        assert!(t.remove_path("/etc/passwd"));
        assert!(t.get("/etc/passwd").is_none());
        assert!(t.get("/etc/shadow").is_some());
        assert!(!t.remove_path("/etc/passwd"), "idempotent");
    }

    #[test]
    fn remove_path_unlinks_the_newest_child_from_the_parent_head() {
        let mut t = Tree::new();
        t.add_path(file("/etc/passwd"));
        t.add_path(file("/etc/shadow"));
        let etc = t.get("/etc").unwrap();
        let shadow = t.get("/etc/shadow").unwrap();
        assert_eq!(
            t.children(etc).first(),
            Some(&shadow),
            "newest child is first_child"
        );
        assert!(t.remove_path("/etc/shadow"));
        assert!(t.get("/etc/shadow").is_none());
        assert!(t.get("/etc/passwd").is_some());
        assert_eq!(
            t.children(etc),
            vec![t.get("/etc/passwd").unwrap()],
            "the parent's first_child chain is still walkable"
        );
    }

    #[test]
    fn remove_path_drops_the_whole_subtree() {
        let mut t = Tree::new();
        t.add_path(file("/etc/sub/a"));
        t.add_path(file("/etc/passwd"));
        assert!(t.remove_path("/etc/sub"));
        assert!(t.get("/etc/sub/a").is_none());
        assert!(t.get("/etc/passwd").is_some());
    }

    #[test]
    fn path_of_the_root_is_a_slash() {
        let t = Tree::new();
        assert_eq!(t.path_of(t.root()), "/");
    }

    /// Dive `FileNode.GetSize`: a file keeps its own size, a directory
    /// rolls its subtree up, and a removed child never counts upward.
    #[test]
    fn subtree_sizes_roll_up_files_and_skip_removed_children() {
        let mut t = Tree::new();
        t.add_path(sized("/bin/busybox", 100));
        t.add_path(sized("/bin/sh", 0));
        t.add_path(sized("/etc/hosts", 7));
        let hosts = t.get("/etc/hosts").unwrap();
        t.node_mut(hosts).diff = DiffType::Removed;
        let sizes = t.subtree_sizes();
        let at = |p: &str| sizes[t.get(p).unwrap()];
        assert_eq!(at("/bin/busybox"), 100, "a file keeps its own size");
        assert_eq!(at("/bin/sh"), 0, "a symlink carries no payload");
        assert_eq!(at("/bin"), 100, "the directory rolls its files up");
        assert_eq!(at("/"), 100, "…and a removed child never counts upward");
        assert_eq!(at("/etc/hosts"), 7, "a removed file still shows its size");
        assert_eq!(at("/etc"), 0, "…but its parent no longer sums it");
    }

    /// The other half of dive's rule: a removed DIRECTORY still
    /// accumulates what it used to hold.
    #[test]
    fn a_removed_directory_accumulates_what_it_used_to_hold() {
        let mut t = Tree::new();
        t.add_path(sized("/opt/app.bin", 50));
        let opt = t.get("/opt").unwrap();
        t.node_mut(opt).diff = DiffType::Removed;
        let sizes = t.subtree_sizes();
        assert_eq!(sizes[opt], 50, "the removed dir shows its old contents");
        // `compare` marks a removed subtree node by node, so the root no
        // longer sums it — the image's live total is what the roll-up reads.
        assert_eq!(sizes[t.root()], 0, "…but it never counts upward again");
    }
}
