//! The file-tree arena: layers store their filesystem as indices into one
//! `Vec<Node>` instead of pointer-linked boxes. Dive's algorithms port
//! cleanly onto this; a literal port of its pointer tree does not.
use super::{DiffType, Entry, Kind};

/// Index into `Tree::nodes`. `0` is always the root.
pub type NodeId = usize;

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug, Default)]
pub struct Tree {
    nodes: Vec<Node>,
    root: NodeId,
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

    fn file(path: &str) -> Entry {
        Entry {
            path: path.to_string(),
            size: 10,
            mode: 0o644,
            uid: 0,
            gid: 0,
            kind: Kind::File,
            link: None,
            hash: 7,
        }
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
}
