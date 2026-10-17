//! The image-layer engine: parse a `docker save` archive into per-layer
//! file trees, diff them the way dive does, and score efficiency.
//!
//! This module is PURE — no `bollard`, no `tokio`, no views — so every rule
//! is unit-testable against synthesised archives. See the design spec
//! §5 for the ported dive semantics.
pub mod archive;
pub mod diff;
pub mod tree;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek};

pub use archive::parse;
pub use diff::{Compare, Comparer, compare, stack};
pub use tree::{Node, NodeId, Tree};

/// What a node's content is, relative to the comparison base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffType {
    Unmodified,
    Modified,
    Added,
    Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Hardlink,
    Other,
}

/// One archive entry (a file, directory, symlink or hardlink).
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Absolute, cleaned; a whiteout's `.wh.` prefix is already stripped.
    pub path: String,
    pub size: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub kind: Kind,
    /// Symlink or hardlink target.
    pub link: Option<String>,
    /// Content hash of the payload (0 for non-files).
    pub hash: u64,
}

impl Entry {
    /// Equal only when type, hash, mode, uid and gid all match
    /// (dive `file_info.go:111`): a pure chmod or chown is a modification.
    pub fn same_content(&self, other: &Entry) -> bool {
        self.kind == other.kind
            && self.hash == other.hash
            && self.mode == other.mode
            && self.uid == other.uid
            && self.gid == other.gid
    }
}

/// One layer: a history command plus its filesystem tree.
#[derive(Clone, Debug)]
pub struct Layer {
    /// Position in the archive (0 = base layer).
    pub index: usize,
    /// The `rootfs.diff_ids` entry this layer maps to.
    pub id: String,
    /// History `created_by`.
    pub command: String,
    /// Total bytes of file payload this layer adds (excludes tar headers
    /// and padding).
    pub size: u64,
    pub tree: Tree,
}

#[derive(Clone, Debug, Default)]
pub struct LayerSet {
    pub layers: Vec<Layer>,
}

/// One of the "inefficient" paths dive lists.
#[derive(Clone, Debug, PartialEq)]
pub struct InefficientPath {
    /// How many layers wrote the path.
    pub count: usize,
    /// Cumulative size across those layers.
    pub wasted: u64,
    pub path: String,
}

/// The finished analysis the window renders.
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub layers: Vec<Layer>,
    /// `min_seen_sum / total_seen_sum`, 0.0..=1.0 (1.0 when undefined).
    pub efficiency: f64,
    /// Potential wasted space: the inefficient paths' cumulative sizes.
    pub wasted_bytes: u64,
    /// Ascending by `wasted`.
    pub inefficient: Vec<InefficientPath>,
}

/// Parse a `docker save` archive into ordered layers (design §5.3).
pub fn analyze(reader: impl Read + Seek) -> anyhow::Result<LayerSet> {
    archive::parse(reader)
}

/// The content hash every layer entry is keyed by (`DefaultHasher`, design
/// D5): hashes are only ever compared within one analysis, so cross-run
/// stability does not matter.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}
