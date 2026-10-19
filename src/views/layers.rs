//! The Layers window: the image's layers on the left, the selected layer's
//! colour-coded change tree on the right, and the efficiency details at the
//! bottom. Mirrors the Explorer's window shape (two `Table`s over
//! `RowModel`s, a `Drop`-cleared reopen flag) but is a separate window —
//! the container-based Image Explorer is untouched. A tvision-rs
//! `Splitter` draws the draggable divider between the two areas (its two
//! placeholder panes carry the 40/60 split; the content children stay
//! flat window children, which is what upstream focus/Tab traversal
//! expects), and the COMMAND/PATH columns stretch (flex) to fill whatever
//! their pane leaves.
use crate::core::{AppCore, TaskReq};
use crate::domain::layers::{Analysis, Compare, Comparer, DiffType, Kind, Tree};
use crate::widgets::resource_table::{RowModel, TableRow, to_spec};
use crate::widgets::{Align, Column, FilterBar, SharedLine};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use tokio::sync::mpsc;
use tv::event::{Event, Key};
use tv::widgets::{SelectionMode, SortDir, Table};
use tv::{
    Constraints, Context, HelpCtx, Rect, Role, ScrollBar, Splitter, View, ViewState, Window,
    delegate,
};

/// Help context while a Layers window is focused.
pub const HELP_CTX: HelpCtx = HelpCtx::custom("tvdocker.layers");

/// Window-menu number for the Layers window (1–6 resources, 7 Details,
/// 8 Logs, 9 Shell, 10 Explorer, 11 Diagnostics).
pub const WINDOW_NUMBER: i16 = 12;

/// The persisted column-layout key for the layer table.
const LAYER_TABLE_KEY: &str = "layers.layers";
/// The persisted column-layout key for the change tree.
const TREE_TABLE_KEY: &str = "layers.tree";

/// The layer table's columns: CMP and SIZE are fixed, COMMAND stretches
/// into every cell they leave (flex), never below 12 cells — a pane
/// narrower than the floor clips and the table still scrolls. The width
/// comes from the table's own layout pass, so it follows resizes AND
/// splitter drags without re-deriving the set.
fn layer_columns() -> Vec<Column> {
    vec![
        Column::new("CMP", 3, Align::Left),
        Column::new("SIZE", 8, Align::Right),
        Column::new("COMMAND", 0, Align::Left).flex(1).min_width(12),
    ]
}

/// The change tree's columns for a `width`-wide pane: PATH stretches into
/// whatever the fixed columns leave (flex), and the attribute columns —
/// dive's UID:GID and Permission — appear only when a readable path still
/// fits beside them (dive drops the attributes entirely in a constrained
/// layout). The 20-cell PATH floor is a COLUMN-SET rule (the attribute
/// thresholds below), not a render floor: the table body leaves its
/// first cell blank (upstream `TEXT_X`), so at render time PATH absorbs
/// the one-cell squeeze exactly like it always did.
fn tree_columns(width: i32) -> Vec<Column> {
    const CHANGE: i32 = 2;
    const SIZE: i32 = 8;
    const UID_GID: i32 = 7;
    const PERM: i32 = 10;
    // A PATH narrower than 20 cells truncates most names to an ellipsis
    // once the indent and connector are in front of them, so the
    // attribute columns only claim their slice when 20 cells still stay.
    const MIN_PATH: i32 = 20;
    let mut cols = vec![
        Column::new("PATH", 0, Align::Left).flex(1),
        Column::new("CHANGE", CHANGE, Align::Left),
        Column::new("SIZE", SIZE, Align::Right),
    ];
    if width >= MIN_PATH + CHANGE + SIZE + PERM + UID_GID {
        cols.push(Column::new("UID:GID", UID_GID, Align::Right));
    }
    if width >= MIN_PATH + CHANGE + SIZE + PERM {
        cols.push(Column::new("PERM", PERM, Align::Left));
    }
    cols
}

/// Child rectangles for a Layers window of `size` (frame included), with
/// the layer/tree divider at window-client column `mid`: a details line,
/// two filter bars, the two tables over the middle — each giving its
/// right-most client column to its vertical scroll bar (the
/// [`crate::widgets::scroll_rects`] body+bar convention) — and the
/// inefficient-paths strip above the bottom border. `mid` is the divider
/// column the [`Splitter`] resolved (its left pane spans columns
/// `1..mid`), so dragging the divider re-derives every middle rect.
fn layers_rects(size: tv::Point, mid: i32) -> LayersRects {
    let right = (size.x - 1).max(1); // exclusive client right (frame = size.x-1)
    let bottom = (size.y - 1).max(1); // exclusive client bottom (frame = size.y-1)
    let mid = mid.clamp(1, right); // the layer/tree divider column
    let first_table = 3.min(bottom); // filters end at row 3 — no blank line (the explorer's formula)
    // `bottom - 3`: the strip needs its header AND two path rows (the
    // design's "list of inefficient paths"); `bottom - 2` left room for a
    // single path, clipping every entry past the first at any window size.
    let details_y = (bottom - 3).max(first_table);
    let last_table = details_y.min(bottom);
    // One column per bar, taken from its own table's right edge; the
    // `.max(…)`/`.min(…)` pairs keep every rect ordered (never inverted,
    // never past the client) even at degenerate widths. The left pane
    // spans columns `1..mid` (its last column is the bar), the tree side
    // starts at `mid + 1` (the divider owns `mid`).
    let layer_body_right = (mid - 1).max(1);
    let layer_bar_right = mid.max(layer_body_right);
    let tree_body_right = (right - 1).max(mid + 1).min(right);
    let tree_bar_right = right.max(tree_body_right);
    LayersRects {
        summary: Rect::new(1, 1, right, 2.min(bottom)),
        split: Rect::new(1, 2.min(bottom), right, details_y),
        layer_filter: Rect::new(1, 2.min(bottom), mid - 1, 3.min(bottom)),
        tree_filter: Rect::new(mid + 1, 2.min(bottom), right, 3.min(bottom)),
        layer_table: Rect::new(1, first_table, layer_body_right, last_table),
        layer_bar: Rect::new(layer_body_right, first_table, layer_bar_right, last_table),
        tree_table: Rect::new(mid + 1, first_table, tree_body_right, last_table),
        tree_bar: Rect::new(tree_body_right, first_table, tree_bar_right, last_table),
        inefficient: Rect::new(1, details_y, right, bottom.max(details_y)),
    }
}

/// The [`layers_rects`] result — one rect per child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayersRects {
    summary: Rect,
    /// The splitter's own rect (mid-independent): summary bottom to strip
    /// top, the full client width.
    split: Rect,
    layer_filter: Rect,
    tree_filter: Rect,
    layer_table: Rect,
    layer_bar: Rect,
    tree_table: Rect,
    tree_bar: Rect,
    inefficient: Rect,
}

/// The Layers window: the layer table on the left, the colour-coded
/// change tree on the right, the inefficient-paths strip at the bottom,
/// two filter bars, and a summary line with the efficiency score.
pub struct LayersWindow {
    win: Window,
    core: Rc<RefCell<AppCore>>,
    task_tx: mpsc::UnboundedSender<TaskReq>,
    /// The image id (the slot key and the `TaskReq` image).
    key: String,
    open: Rc<RefCell<bool>>,
    /// The layout splitter: two placeholder panes between the layer and
    /// tree areas. Its divider replaces the old blank gap column and can
    /// be dragged; the content children stay FLAT and follow the pane
    /// widths through [`Self::reapply_layout`].
    splitter: tv::ViewId,
    /// The splitter's left placeholder pane (its width carries the split).
    layer_pane: tv::ViewId,
    /// The left pane width the content rects were last derived from —
    /// [`Self::split_moved`] compares it against the live pane to detect a
    /// dragged divider.
    split_left: i32,
    layer_table: tv::ViewId,
    /// The layer table's vertical scroll bar (its right-most column).
    layer_bar: tv::ViewId,
    tree_table: tv::ViewId,
    /// The change tree's vertical scroll bar (its right-most column).
    tree_bar: tv::ViewId,
    inefficient_table: tv::ViewId,
    layer_filter: tv::ViewId,
    tree_filter: tv::ViewId,
    summary_id: tv::ViewId,
    summary: Rc<RefCell<String>>,
    /// The layer filter bar's shared sink (`FilterBar` writes it on every
    /// keystroke; the window reads it back in [`redraw`]).
    layer_filter_sink: Rc<RefCell<String>>,
    /// The path filter bar's shared sink (the tree's needle, see
    /// [`Self::visible_rows`]).
    tree_filter_sink: Rc<RefCell<String>>,
    /// The selected layer index (the left table's focused row).
    selected: usize,
    /// Which comparison the change tree shows (rule 5).
    mode: Compare,
    /// Directories the user collapsed (the root is never collapsed).
    collapsed: HashSet<String>,
    /// The column sets this window was BUILT with: a `Table` takes its
    /// column specs once, so `redraw` and the sort step must agree with
    /// the build-time pane width, never the current one (a resize clips
    /// the widest column instead of re-deriving the set).
    layer_columns: Vec<Column>,
    tree_columns: Vec<Column>,
    /// Change types currently hidden (A/R/M/U toggles), indexed by
    /// [`diff_index`] — `DiffType` carries no `Hash`, so a fixed array
    /// stands in for a set.
    hidden: [bool; 4],
    comparer: Comparer,
    /// The flattened rows of the change tree, in DFS order.
    tree_rows: Vec<TreeRow>,
    /// The cached analysis (`None` = still loading), wrapped ONCE per data
    /// change in `Rc` — an event holds a cheap pointer, never a deep copy
    /// of the per-layer trees (finding 1). Like the Explorer's
    /// `Panel::entries`, the window holds the data and only pushes it into
    /// the tables where it has a `Context` (the `redraw_texts` pattern).
    analysis: Option<Rc<Analysis>>,
    /// The last `layers_data_rev` this window mapped (`None` until the
    /// first sync, so a freshly built window always maps whatever the core
    /// has — even a directly-seeded test core that never bumped the
    /// counter). Progress ticks bump `layers_rev`, NOT this — so they
    /// never trigger a re-read.
    last_data_rev: Option<u64>,
    /// `redraw` runs only while this is set (finding 1's gate): set on a
    /// data-rev change, a `selected` change, a state-mutating window key,
    /// a size reconcile and — while the slot is empty — a new progress
    /// byte count; cleared by `redraw` itself. Filter needles are compared
    /// separately in `handle_event` (the `FilterBar` children write their
    /// sinks inside `win.handle_event`, which is why the gate sits last).
    dirty: bool,
    /// The last byte count seen for the empty slot's "Analyzing …" line.
    last_progress: u64,
    /// The layer/PATH needles as of the last `redraw` — the gate compares
    /// the sinks against these to detect a typed filter change.
    last_layer_needle: String,
    last_tree_needle: String,
}

/// One table of the window: a `Table` wired to `bar` (the vertical scroll
/// bar in its right-most column, `None` for the bar-less inefficient
/// strip) and seeded with `rows`.
fn make_table(
    rect: Rect,
    key: &str,
    columns: Vec<Column>,
    rows: Vec<TableRow>,
    bar: Option<tv::ViewId>,
) -> Table {
    let specs = columns.iter().map(to_spec).collect();
    let mut table = Table::new(rect, specs, bar)
        .with_selection_mode(SelectionMode::Single)
        .with_model(Box::new(RowModel::new(rows)));
    table.state_mut().options.selectable = true;
    if let Some(cfg) = crate::domain::config::TvdockerConfig::load()
        .0
        .column_layout(key)
        .cloned()
    {
        table.set_column_layout(&cfg.to_states());
    }
    table
}

/// The layer-table rows: a compare marker, the size and the command —
/// kept when `needle` fuzzy-matches the command cell (the
/// [`crate::views::resource::fuzzy_keep_row`] matcher, restricted to the
/// row's search text; empty matches all).
fn layer_rows(analysis: &Analysis, selected: usize, needle: &str) -> Vec<TableRow> {
    analysis
        .layers
        .iter()
        .filter(|l| {
            needle.is_empty()
                || crate::views::resource::fuzzy_keep_row(
                    &TableRow::new(String::new(), vec![l.command.clone()]),
                    needle,
                )
        })
        .map(|l| {
            let marker = if l.index == selected { "▏" } else { " " };
            TableRow::new(
                format!("L{}", l.index),
                vec![
                    marker.to_string(),
                    crate::views::resource::format_size(l.size as i64),
                    l.command.clone(),
                ],
            )
        })
        .collect()
}

/// The inefficient-paths strip rows: count, wasted bytes and path.
fn inefficient_rows(analysis: &Analysis) -> Vec<TableRow> {
    analysis
        .inefficient
        .iter()
        .map(|p| {
            TableRow::new(
                p.path.clone(),
                vec![
                    p.count.to_string(),
                    crate::views::resource::format_size(p.wasted as i64),
                    p.path.clone(),
                ],
            )
        })
        .collect()
}

/// A dense index for [`DiffType`] (the [`LayersWindow::hidden`] array —
/// `DiffType` does not implement `Hash`).
fn diff_index(diff: DiffType) -> usize {
    match diff {
        DiffType::Unmodified => 0,
        DiffType::Modified => 1,
        DiffType::Added => 2,
        DiffType::Removed => 3,
    }
}

/// One row of the change tree (the flatten result, kept so the key
/// handlers in Task 10 can re-filter without re-walking the tree).
#[derive(Clone, Debug)]
pub struct TreeRow {
    pub path: String,
    pub diff: DiffType,
    pub is_dir: bool,
    pub depth: usize,
}

/// The DFS flatten the window renders (depth for indentation, `is_dir` for
/// the expand/collapse keys added in Task 10).
pub fn flatten(tree: &Tree) -> Vec<TreeRow> {
    tree.dfs()
        .into_iter()
        .filter(|id| *id != tree.root())
        .map(|id| {
            let node = tree.node(id);
            let path = tree.path_of(id);
            let depth = path.matches('/').count().saturating_sub(1);
            TreeRow {
                path,
                diff: node.diff,
                is_dir: tree.is_dir(id),
                depth,
            }
        })
        .collect()
}

/// Dive's change letters, tinted by the theme roles (design §7): the
/// upstream `Role` is a closed enum, so Added/Removed/Modified map onto
/// Success/Error/Warning. The row role replaces only the normal colour —
/// focus and selection still win.
pub fn role_for(diff: DiffType) -> Option<Role> {
    match diff {
        DiffType::Added => Some(Role::Success),
        DiffType::Removed => Some(Role::Error),
        DiffType::Modified => Some(Role::Warning),
        DiffType::Unmodified => None,
    }
}

/// The change letter shown in the tree's CHANGE column.
fn letter_for(diff: DiffType) -> char {
    match diff {
        DiffType::Added => '+',
        DiffType::Removed => '-',
        DiffType::Modified => '~',
        DiffType::Unmodified => ' ',
    }
}

/// No ancestor directory of `path` (root excluded) is collapsed — a
/// nearest-to-root walk over the path prefixes, so collapsing `/usr/lib`
/// hides `/usr/lib/jvm/bin` too. (Free of `self` so `redraw` can run the
/// filter pass while `self.comparer` is mutably borrowed.)
fn expanded_by(collapsed: &HashSet<String>, path: &str) -> bool {
    let mut ancestor = path;
    while let Some(i) = ancestor.rfind('/') {
        ancestor = &ancestor[..i];
        if ancestor.is_empty() {
            return true; // the root is never collapsed
        }
        if collapsed.contains(ancestor) {
            return false;
        }
    }
    true
}

/// The collapse-state / change-type / path-needle pass over the flattened
/// rows — the free core of [`LayersWindow::visible_rows`] (free of `self`
/// so `redraw` can call it while `self.comparer` is mutably borrowed).
fn visible_rows_in(
    rows: &[TreeRow],
    collapsed: &HashSet<String>,
    hidden: &[bool; 4],
    needle: &str,
) -> Vec<TreeRow> {
    let needle = needle.to_lowercase();
    rows.iter()
        .filter(|r| !hidden[diff_index(r.diff)])
        .filter(|r| expanded_by(collapsed, &r.path))
        .filter(|r| needle.is_empty() || r.path.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

/// `TreeRow`s as `TableRow`s: dive's tree rendering — `├─`/`└─`
/// connectors, `│` continuations under an ancestor that has more siblings
/// below it, a `→ target` suffix for symlinks, and the `⊕ ` collapse
/// marker in front of a COLLAPSED directory (finding 3, spec §7;
/// `file_tree.go`'s `renderStringTreeBetween`/`collapsedItem`) — plus the
/// change letter, the size, the uid:gid and the rwx permissions, tinted
/// by [`role_for`]. Size/permissions/ids come from `tree` — the comparison
/// tree the rows were flattened from, not the selected layer's tar — and
/// `compare` back-fills a `Removed` node with its `Entry`, so a removed
/// row still shows what it used to be. The collapse state is passed as
/// the window's `collapsed` set (rather than a per-row flag) so
/// `flatten`/`TreeRow` stay collapse-unaware. Connectors describe the
/// VISIBLE rows, so collapsing a directory redraws them.
pub fn tree_rows_for(
    rows: &[TreeRow],
    tree: &Tree,
    collapsed: &HashSet<String>,
    columns: &[Column],
) -> Vec<TableRow> {
    // One flag per depth level: does that ancestor have a sibling below
    // it? A row draws `│   ` per true ancestor and `    ` per false one.
    // Dive's rolled-up sizes: a directory shows the sum of the files
    // under it (deleted ones excluded), computed once for the whole tree.
    let sizes = tree.subtree_sizes();
    let mut ancestors: Vec<bool> = Vec::new();
    let mut out = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        ancestors.truncate(r.depth);
        // The next row that is not this row's descendant decides the
        // connector: a same-depth sibling means `├─`, anything shallower
        // (or nothing) means this is the last child, `└─`.
        let sibling_below = rows[i + 1..]
            .iter()
            .find(|n| n.depth <= r.depth)
            .is_some_and(|n| n.depth == r.depth);
        let indent: String = ancestors
            .iter()
            .map(|keep| if *keep { "│   " } else { "    " })
            .collect();
        ancestors.push(sibling_below);
        let connector = if sibling_below { "├─ " } else { "└─ " };
        let entry = tree.get(&r.path).and_then(|id| tree.node(id).entry.clone());
        let size = tree
            .get(&r.path)
            .map(|id| crate::views::resource::format_size(sizes[id] as i64))
            .unwrap_or_default();
        let ids = entry
            .as_ref()
            .map(|e| format!("{}:{}", e.uid, e.gid))
            .unwrap_or_default();
        let perm = entry
            .as_ref()
            .map(|e| perm_string(e.mode, e.kind))
            .unwrap_or_default();
        let name = r.path.rsplit('/').next().unwrap_or(&r.path);
        // Only a collapsed DIRECTORY is flagged: with the subtree hidden,
        // the marker is what tells collapsed rows apart.
        let marker = if r.is_dir && collapsed.contains(&r.path) {
            "⊕ "
        } else {
            ""
        };
        let target = entry
            .as_ref()
            .and_then(|e| e.link.clone())
            .map(|t| format!(" → {t}"))
            .unwrap_or_default();
        // One cell per column of THIS table: a pane too narrow for the
        // attribute columns has no UID:GID/PERM cells either, so a row can
        // never carry a value its table has no column for.
        let cells = columns
            .iter()
            .map(|c| match c.label.as_str() {
                "PATH" => format!("{indent}{connector}{marker}{name}{target}"),
                "CHANGE" => letter_for(r.diff).to_string(),
                "SIZE" => size.clone(),
                "UID:GID" => ids.clone(),
                "PERM" => perm.clone(),
                _ => String::new(),
            })
            .collect();
        out.push(TableRow::new(r.path.clone(), cells).with_opt_role(role_for(r.diff)));
    }
    out
}

/// One entry's permissions the way dive's Permission column shows them:
/// the kind letter followed by the nine rwx bits (`drwxr-xr-x`).
fn perm_string(mode: u32, kind: Kind) -> String {
    let mut out = String::with_capacity(10);
    out.push(match kind {
        Kind::Dir => 'd',
        Kind::Symlink => 'l',
        Kind::Hardlink => 'h',
        Kind::File => '-',
        Kind::Other => '?',
    });
    for shift in (0..9).rev() {
        const BITS: [char; 3] = ['r', 'w', 'x'];
        out.push(if mode & (1 << shift) != 0 {
            BITS[(8 - shift) as usize % 3]
        } else {
            '-'
        });
    }
    out
}

/// A layer id as the summary line shows it: the algorithm prefix plus the
/// first 12 hex characters (docker's short-id width).
fn short_id(id: &str) -> String {
    let (algo, hex) = match id.split_once(':') {
        Some((algo, hex)) => (Some(algo), hex),
        None => (None, id),
    };
    let short: String = hex.chars().take(12).collect();
    match algo {
        Some(algo) => format!("{algo}:{short}"),
        None => short,
    }
}

/// Build one window. The initial analysis request is sent here ONLY when
/// the core has no finished analysis for the key (spec D9: image ids are
/// content-addressed, so a cached analysis can never be stale) — build
/// cannot fail, and an empty table simply renders no rows until the core
/// caches the finished analysis.
pub fn build(
    core: Rc<RefCell<AppCore>>,
    bounds: Rect,
    key: String,
    label: String,
    open: Rc<RefCell<bool>>,
    task_tx: mpsc::UnboundedSender<TaskReq>,
) -> LayersWindow {
    let title = format!("Layers — {label}");
    let mut win = Window::new(bounds, Some(title), WINDOW_NUMBER);
    let size = win.state().size;
    // Provisional mid — re-derived below, once the splitter resolves its
    // panes; only the mid-independent rects (summary/strip/split) are
    // read before that.
    let r = layers_rects(size, (size.x * 2) / 5);

    let summary = Rc::new(RefCell::new(String::new()));
    let layer_sink = Rc::new(RefCell::new(String::new()));
    let tree_sink = Rc::new(RefCell::new(String::new()));
    // Keep the sinks on the window too: the `FilterBar`s write them, the
    // window reads the needles back for filtering.
    let layer_filter_sink = Rc::clone(&layer_sink);
    let tree_filter_sink = Rc::clone(&tree_sink);

    // Seed all three tables with what the first `redraw` would produce
    // (the ResourceWindow's build-seeded `initial_model`): the redraw now
    // runs AFTER the child routing, so without this a fresh window's
    // first event would land on empty tables. This is the ONE deep clone
    // per open — the `Rc` wrap keeps every later event cheap.
    let (seeded, data_rev, needs_request) = {
        let core = core.borrow();
        let slot = core.image_layers.get(&key);
        (
            slot.cloned().flatten().map(Rc::new),
            core.layers_data_rev,
            // Finding 1.4: a cached analysis is never re-requested — only
            // an absent (or still loading) slot starts the `docker save`
            // stream (and its per-chunk progress) again.
            slot.is_none_or(|analysis| analysis.is_none()),
        )
    };
    let mut comparer = Comparer::new();
    // The layer columns need no width: COMMAND stretches (flex). The tree's
    // attribute columns (UID:GID, PERM) still follow the pane — decided
    // against the width the SOLVER grants the right pane.
    let layer_cols = layer_columns();

    // --- The splitter: two placeholder panes over the window's middle
    // region (40/60 weights, the old fixed split as the default). The
    // CONTENT children below stay FLAT window children — upstream focus
    // and Tab traversal are a direct-children affair (`focus_descendant`'s
    // doc: "this focuses the link within its owning group, not the full
    // ancestor chain") — so the splitter is the layout/divider device:
    // `reapply_layout` derives every middle rect from the resolved left
    // pane width, and `split_moved` catches a dragged divider. Inserted
    // FIRST, it also draws behind the content that overlaps its region.
    let mut split = Splitter::cols();
    let layer_pane = split.insert(
        Box::new(tv::Group::new(Rect::new(0, 0, 1, 1))),
        Constraints::weight(2),
    );
    split.insert(
        Box::new(tv::Group::new(Rect::new(0, 0, 1, 1))),
        Constraints::weight(3),
    );
    split.change_bounds(r.split);
    let split_left = split
        .find_mut(layer_pane)
        .map_or(0, |pane| pane.state().size.x);
    // The real divider column: the splitter's left edge plus the width
    // the solver granted the left pane (its own coordinate space is
    // 0-based, so the divider sits one column past the pane).
    let r = layers_rects(size, r.split.a.x + split_left);
    let tree_width = r.tree_table.b.x - r.tree_table.a.x;
    let tree_cols = tree_columns(tree_width);

    // Seed all three tables with what the first `redraw` would produce
    // (the ResourceWindow's build-seeded `initial_model`): the redraw now
    // runs AFTER the child routing, so without this a fresh window's
    // first event would land on empty tables. This is the ONE deep clone
    // per open — the `Rc` wrap keeps every later event cheap.
    let (seed_layers, seed_tree, seed_inefficient) = match seeded.as_deref() {
        Some(a) if !a.layers.is_empty() => {
            let tree = comparer.tree(&a.layers, Compare::Single, 0).clone();
            (
                layer_rows(a, 0, ""),
                tree_rows_for(&flatten(&tree), &tree, &HashSet::new(), &tree_cols),
                inefficient_rows(a),
            )
        }
        _ => (Vec::new(), Vec::new(), Vec::new()),
    };

    let splitter = win.insert_child(Box::new(split));
    let summary_id = win.insert_child(Box::new(SharedLine::new(
        r.summary,
        Rc::clone(&summary),
        Role::Info,
    )));
    let layer_filter = win.insert_child(Box::new(FilterBar::new(
        r.layer_filter,
        layer_sink,
        "layer filter (/)",
    )));
    let tree_filter = win.insert_child(Box::new(FilterBar::new(
        r.tree_filter,
        tree_sink,
        "path filter (/)",
    )));
    // Bars FIRST (the `build_scroll_window` pattern): the table wires
    // itself to the bar's id and publishes its row range to it.
    let layer_bar = win.insert_child(Box::new(ScrollBar::new(r.layer_bar)));
    let layer_table = win.insert_child(Box::new(make_table(
        r.layer_table,
        LAYER_TABLE_KEY,
        layer_cols.clone(),
        seed_layers,
        Some(layer_bar),
    )));
    let tree_bar = win.insert_child(Box::new(ScrollBar::new(r.tree_bar)));
    let tree_table = win.insert_child(Box::new(make_table(
        r.tree_table,
        TREE_TABLE_KEY,
        tree_cols.clone(),
        seed_tree,
        Some(tree_bar),
    )));
    let inefficient_table = win.insert_child(Box::new(make_table(
        r.inefficient,
        "layers.inefficient",
        vec![
            Column::new("COUNT", 6, Align::Right),
            Column::new("WASTED", 10, Align::Right),
            Column::new("PATH", 40, Align::Left),
        ],
        seed_inefficient,
        None,
    )));

    let mut window = LayersWindow {
        win,
        core,
        task_tx,
        key,
        open,
        splitter,
        layer_pane,
        split_left,
        layer_table,
        layer_bar,
        tree_table,
        tree_bar,
        inefficient_table,
        layer_filter,
        tree_filter,
        layer_columns: layer_cols,
        tree_columns: tree_cols,
        summary_id,
        summary,
        layer_filter_sink,
        tree_filter_sink,
        selected: 0,
        mode: Compare::Single,
        collapsed: HashSet::new(),
        hidden: [false; 4],
        comparer,
        tree_rows: Vec::new(),
        analysis: seeded,
        last_data_rev: Some(data_rev),
        dirty: true, // a freshly built window renders on its first event
        last_progress: 0,
        last_layer_needle: String::new(),
        last_tree_needle: String::new(),
    };
    // The data was mapped by the seed read above; the sync only picks up
    // the progress bookkeeping. `write_summary` so a fresh window renders
    // the line even before its first event.
    window.sync_from_core();
    window.write_summary();
    if needs_request {
        // The initial analysis request (the Explorer's build-time request
        // precedent): the opener routes the command only, so the window asks
        // the worker itself — keyed AND image-typed by the image id.
        let _ = window.task_tx.send(TaskReq::AnalyzeImageLayers {
            key: window.key.clone(),
            image: window.key.clone(),
        });
    }
    window
}

impl LayersWindow {
    /// A table by id, as a `&mut Table` (the Explorer's `table_mut`).
    fn table_mut(&mut self, id: tv::ViewId) -> Option<&mut Table> {
        self.win
            .child_mut(id)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<Table>())
    }

    /// Ask the pump to focus the layer table once this window is wired in
    /// (the Explorer's `request_initial_focus`): a non-modal window's
    /// `reset_current` never runs, so the pump would otherwise settle on
    /// the last-inserted selectable child (an empty side table).
    pub fn request_initial_focus(&mut self, ctx: &mut Context) {
        ctx.request_focus(self.layer_table);
    }

    /// Cache a fresh analysis (or the loading state) — the Explorer's
    /// stale-response discipline, keyed on `layers_data_rev` (finding 1.2):
    /// a per-chunk `LayerProgress` tick bumps only `layers_rev`, so it
    /// costs this window one integer read, never a deep copy of the
    /// per-layer trees. No `Context` here: [`redraw`] pushes the cached
    /// data into the tables (and writes the summary line).
    fn sync_from_core(&mut self) {
        let core = self.core.borrow();
        // Unchanged since the last map: nothing to re-read. `last_data_rev`
        // is `None` until the FIRST sync, so a freshly built window always
        // maps — even when a test seeds the slot without bumping the
        // counter (`build` seeds it for windows it builds itself).
        if self.last_data_rev != Some(core.layers_data_rev) {
            self.last_data_rev = Some(core.layers_data_rev);
            // Deep-clone ONLY when the data actually changed: one `Rc`
            // wrap per data change, then every event holds a pointer.
            let analysis = core
                .image_layers
                .get(&self.key)
                .cloned()
                .flatten()
                .map(Rc::new);
            let progress = core.layers_progress.get(&self.key).copied().unwrap_or(0);
            drop(core);
            if let Some(a) = &analysis {
                self.selected = self.selected.min(a.layers.len().saturating_sub(1));
            }
            self.analysis = analysis;
            self.last_progress = progress;
            self.dirty = true; // the models (and the summary) must rebuild
            return;
        }
        // Data unchanged: while the slot is empty, a NEW byte count still
        // marks the window dirty so the "Analyzing …" line updates.
        if self.analysis.is_none() {
            let progress = core.layers_progress.get(&self.key).copied().unwrap_or(0);
            drop(core);
            if progress != self.last_progress {
                self.last_progress = progress;
                self.dirty = true;
            }
        }
    }

    /// The summary line (`SharedLine`, row 1): the compare-mode indicator
    /// plus the totals, or the loading line with the current byte count.
    /// Written from `build` and every `redraw` — so it follows the data,
    /// the mode and the progress ticks through the same dirty gate the
    /// tables use.
    ///
    /// Finding 2's ruling: the window TITLE cannot change at runtime in
    /// tvision-rs 0.17 (`Window::set_title` is `pub(crate)` with no
    /// external path to the title), so spec §10's "the toggle changes the
    /// title" acceptance is served by row 1, which is always visible.
    fn write_summary(&self) {
        let mode = match self.mode {
            Compare::Single => "Current layer",
            Compare::Aggregated => "Aggregated",
        };
        *self.summary.borrow_mut() = match &self.analysis {
            Some(a) => {
                // dive's Layer Details pane names the selected layer by
                // its diff id; the summary row carries it instead.
                let layer = a
                    .layers
                    .get(self.selected)
                    .map(|l| format!(" · L{} {}", l.index, short_id(&l.id)))
                    .unwrap_or_default();
                format!(
                    " {mode}{layer} · Total {}   Efficiency {:.1}%   Potential wasted space {}",
                    crate::views::resource::format_size(
                        a.layers.iter().map(|l| l.size as i64).sum()
                    ),
                    a.efficiency * 100.0,
                    crate::views::resource::format_size(a.wasted_bytes as i64),
                )
            }
            None => {
                let progress = self
                    .core
                    .borrow()
                    .layers_progress
                    .get(&self.key)
                    .copied()
                    .unwrap_or(0);
                format!(
                    " {mode} · Analyzing {} ({})…",
                    crate::views::resource::format_size(progress as i64),
                    self.key
                )
            }
        };
    }

    /// Show `path`'s subtree again (Space / `+`).
    pub fn expand(&mut self, path: &str) {
        if self.collapsed.remove(path) {
            self.dirty = true; // the subtree returns to the table
        }
    }

    /// Hide `path`'s subtree (`-`).
    pub fn collapse(&mut self, path: &str) {
        if self.collapsed.insert(path.to_string()) {
            self.dirty = true;
        }
    }

    /// Flip change type `diff` between shown and hidden (A/R/M/U).
    pub fn toggle_diff(&mut self, diff: DiffType) {
        let i = diff_index(diff);
        self.hidden[i] = !self.hidden[i];
        self.dirty = true;
    }

    /// Switch between the two dive compare modes (Ctrl+T): the selected
    /// layer against its stack, or layer 0 against the whole stack.
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            Compare::Single => Compare::Aggregated,
            Compare::Aggregated => Compare::Single,
        };
        self.dirty = true; // a different comparer key → a different tree
    }

    /// The rows the collapse state, the change-type filters and the path
    /// filter allow through, in DFS order — ONE pass over the flattened
    /// rows, the single source [`redraw`] feeds to [`tree_rows_for`]. A
    /// directory hides its whole subtree when it is collapsed; the root
    /// (`/`) is never collapsed. (The body is the free
    /// [`visible_rows_in`] so `redraw` can run it while `self.comparer`
    /// is mutably borrowed.)
    pub fn visible_rows(&self) -> Vec<TreeRow> {
        let needle = self.tree_filter_sink.borrow();
        visible_rows_in(&self.tree_rows, &self.collapsed, &self.hidden, &needle)
    }

    /// The tree path of the right table's focused row (`None` while
    /// nothing is rendered yet). The row's KEY is the path, so this stays
    /// correct under a Ctrl+O sort — an index into [`Self::visible_rows`]
    /// would not.
    fn focused_tree_path(&mut self) -> Option<String> {
        self.table_mut(self.tree_table)?.focused_key().map(|k| k.0)
    }

    /// The layer index of the layer table's focused row — keyed on the row id
    /// (`"L{n}"`), so it survives both column sorting and the fuzzy layer
    /// filter. `None` while the table has no cursor (an empty model).
    fn focused_layer(&mut self) -> Option<usize> {
        let key = self.table_mut(self.layer_table)?.focused_key()?.0; // e.g. "L2"
        key.strip_prefix('L')?.parse().ok()
    }

    /// Is `id` (or one of its descendants) the focused view? (The
    /// Explorer's `is_focused`; `child_mut` because focus lives on the
    /// child's own state.)
    fn is_focused(&mut self, id: tv::ViewId) -> bool {
        self.win
            .child_mut(id)
            .is_some_and(|v| v.state().state.focused)
    }

    /// Is one of the two filter bars focused? (gate for the printables —
    /// a focused bar swallows them for typing instead, the Explorer's
    /// `filter_focused`.)
    fn filter_focused(&mut self) -> bool {
        self.is_focused(self.layer_filter) || self.is_focused(self.tree_filter)
    }

    /// Ctrl+O: step the focused table's sort to the next column, ascending
    /// (the Explorer's `cycle_sort`, without the hidden-column probe — these
    /// tables have no hidden columns).
    fn cycle_sort(&mut self, forward: bool, ctx: &mut Context) {
        let on_tree = self.is_focused(self.tree_table);
        let (id, n) = if on_tree {
            (self.tree_table, self.tree_columns.len())
        } else {
            (self.layer_table, self.layer_columns.len())
        };
        let Some(t) = self.table_mut(id) else { return };
        let step: i64 = if forward { 1 } else { -1 };
        let start = t
            .sort_spec()
            .map_or(if forward { -1 } else { n as i64 }, |(c, _)| c as i64);
        let col = ((start + step).rem_euclid(n as i64)) as usize;
        t.sort_by(col, if forward { SortDir::Asc } else { SortDir::Desc }, ctx);
        self.dirty = true; // the row order changed (the `t` borrow ends here)
    }

    /// `/` (spec §7): send the filter bar of the pane that owns the focus.
    fn focus_active_filter(&mut self, ctx: &mut Context) {
        let on_tree = self.is_focused(self.tree_table);
        let target = if on_tree {
            self.tree_filter
        } else {
            self.layer_filter
        };
        self.win.focus_descendant(target, ctx);
    }

    /// The window's own keys (design §7): expand/collapse, the
    /// compare-mode toggle, the four change-type filters, Ctrl+O's sort
    /// step and `/`'s filter focus. Only mutates state or moves focus —
    /// `handle_event` delegates to the children and redraws after this
    /// returns. The printables are gated on no filter bar being focused
    /// (the Explorer's rule): a focused bar swallows them for typing.
    fn window_keys(&mut self, ev: &mut Event, ctx: &mut Context) {
        let Event::KeyDown(ke) = ev else { return };
        match (ke.key, ke.modifiers.ctrl) {
            // Tab: exactly two stops (the Explorer's Ruling 3). The window
            // intercepts BEFORE the children run, so the nested groups'
            // hierarchical traversal never fires: the focus toggles
            // between the two tables, and a focused filter escapes back
            // to ITS OWN pane's table (Shift+Tab behaves like Tab — tv
            // has no `BackTab` variant).
            (Key::Tab, false) => {
                let target = if self.is_focused(self.tree_filter) {
                    self.tree_table
                } else if self.is_focused(self.layer_filter) || self.is_focused(self.tree_table) {
                    // A filter escapes to its own table; the layer table
                    // crosses to the tree table.
                    self.layer_table
                } else {
                    self.tree_table
                };
                self.win.focus_descendant(target, ctx);
                ev.clear();
            }
            // Ctrl chords the FilterBar never consumes.
            (Key::Char('t'), true) => {
                self.toggle_mode();
                ev.clear();
            }
            (Key::Char('o'), true) => {
                self.cycle_sort(true, ctx);
                ev.clear();
            }
            (key, false) if !self.filter_focused() => match key {
                Key::Char('/') => {
                    self.focus_active_filter(ctx);
                    ev.clear();
                }
                Key::Char(' ') | Key::Char('+') => {
                    if let Some(path) = self.focused_tree_path() {
                        self.expand(&path);
                        ev.clear();
                    }
                }
                Key::Char('-') => {
                    if let Some(path) = self.focused_tree_path() {
                        self.collapse(&path);
                        ev.clear();
                    }
                }
                Key::Char('*') => {
                    if !self.collapsed.is_empty() {
                        self.collapsed.clear();
                        self.dirty = true;
                    }
                    ev.clear();
                }
                Key::Char('a') => {
                    self.toggle_diff(DiffType::Added);
                    ev.clear();
                }
                Key::Char('r') => {
                    self.toggle_diff(DiffType::Removed);
                    ev.clear();
                }
                Key::Char('m') => {
                    self.toggle_diff(DiffType::Modified);
                    ev.clear();
                }
                Key::Char('u') => {
                    self.toggle_diff(DiffType::Unmodified);
                    ev.clear();
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Push the cached analysis into the three tables (needs a `Context`,
    /// so it runs from `handle_event` — the `redraw_texts` pattern — and
    /// only when the dirty gate says something it reads changed, finding
    /// 1.3). Also records the rendered needles and rewrites the summary
    /// line, so a clean window is by definition up to date.
    fn redraw(&mut self, ctx: &mut Context) {
        self.dirty = false;
        self.last_layer_needle = self.layer_filter_sink.borrow().clone();
        self.last_tree_needle = self.tree_filter_sink.borrow().clone();
        self.write_summary();
        // Nothing renderable — still loading, a re-analysis wiped the slot,
        // or a degenerate zero-layer analysis (`Comparer::tree` slices
        // `layers[0..=selected]`). Clear the tables — the explorer's
        // clear-then-repush — so stale rows cannot outlive the summary's
        // "Analyzing" line (Task 8 review carry C).
        let Some(a) = self.analysis.clone().filter(|a| !a.layers.is_empty()) else {
            self.tree_rows.clear();
            for id in [self.layer_table, self.tree_table, self.inefficient_table] {
                if let Some(t) = self.table_mut(id) {
                    t.set_model(Box::new(RowModel::new(Vec::new())), ctx);
                }
            }
            return;
        };
        let rows = layer_rows(&a, self.selected, &self.last_layer_needle);
        if let Some(t) = self.table_mut(self.layer_table) {
            t.set_model(Box::new(RowModel::new(rows)), ctx);
        }
        // `Comparer::tree` borrows `self.comparer` mutably, so the whole
        // rebuild runs in this block and only owned rows leave it: the
        // collapse state, the change-type filters and the path needle
        // re-shape the flattened rows into the visible rows — ONE O(n)
        // pass in `visible_rows_in`, fed straight to `tree_rows_for` (no
        // per-row membership lookup, and NO comparison-tree clone to
        // break the borrow — the field-disjoint borrows are enough).
        let (flat, tree_rows) = {
            let tree = self.comparer.tree(&a.layers, self.mode, self.selected);
            let flat = flatten(tree);
            let visible =
                visible_rows_in(&flat, &self.collapsed, &self.hidden, &self.last_tree_needle);
            (
                flat,
                tree_rows_for(&visible, tree, &self.collapsed, &self.tree_columns),
            )
        };
        self.tree_rows = flat;
        if let Some(t) = self.table_mut(self.tree_table) {
            t.set_model(Box::new(RowModel::new(tree_rows)), ctx);
        }
        if let Some(t) = self.table_mut(self.inefficient_table) {
            t.set_model(Box::new(RowModel::new(inefficient_rows(&a))), ctx);
        }
    }

    /// The left pane's CURRENT width (the splitter re-resolves its panes
    /// on every `change_bounds`, so this is the live divider position).
    fn split_left_width(&mut self) -> Option<i32> {
        self.win
            .find_mut(self.layer_pane)
            .map(|pane| pane.state().size.x)
    }

    /// Did the divider move since the content rects were last applied?
    /// A mouse drag lands inside `win.handle_event`; the keyboard resize
    /// broker applies its nudge after dispatch — both show up here.
    fn split_moved(&mut self) -> bool {
        self.split_left_width() != Some(self.split_left)
    }

    /// Re-derive the child rects for the CURRENT window size and the
    /// CURRENT divider position (the Explorer's `reapply_layout`): bound
    /// the splitter (its panes resolve), read back the left pane's width,
    /// then hand every child its rect from `layers_rects(size, mid)` —
    /// without it the children keep their BUILD-time rects and freeze on
    /// resize or a dragged divider.
    fn reapply_layout(&mut self) {
        let size = self.win.state().size;
        // Pass 1: only the split rect matters for this call (the summary
        // and strip rects are mid-independent), and it lets the panes
        // resolve for the CURRENT window size.
        let r = layers_rects(size, self.split_left);
        if let Some(v) = self.win.child_mut(self.splitter) {
            v.change_bounds(r.split);
        }
        let left = self.split_left_width().unwrap_or(0);
        self.split_left = left;
        // Pass 2: the divider column the resolver actually picked.
        let r = layers_rects(size, r.split.a.x + left);
        for (id, rect) in [
            (self.summary_id, r.summary),
            (self.layer_filter, r.layer_filter),
            (self.tree_filter, r.tree_filter),
            (self.layer_table, r.layer_table),
            (self.layer_bar, r.layer_bar),
            (self.tree_table, r.tree_table),
            (self.tree_bar, r.tree_bar),
            (self.inefficient_table, r.inefficient),
        ] {
            if let Some(v) = self.win.child_mut(id) {
                v.change_bounds(rect);
            }
        }
    }
}

impl Drop for LayersWindow {
    fn drop(&mut self) {
        // The reopen-vs-focus flag (the Explorer/Logs/Details pattern).
        *self.open.borrow_mut() = false;
    }
}

#[delegate(to = win)]
impl View for LayersWindow {
    fn state(&self) -> &ViewState {
        self.win.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.win.state_mut()
    }

    /// Re-derive the child rects whenever the window size changes (zoom /
    /// Size-Move drag / terminal resize).
    fn change_bounds(&mut self, bounds: Rect) {
        self.win.change_bounds(bounds);
        self.reapply_layout();
    }

    /// Every event: run the window's own keys, map a fresh analysis,
    /// delegate to the children, hand the layer cursor to `selected`, then
    /// — only when the dirty gate says something `redraw` reads changed
    /// (finding 1.3) — push the cached rows into the tables (the
    /// Explorer's `sync_from_core` + `redraw_texts`, in the
    /// ResourceWindow's delegate-then-refilter ORDER): the redraw runs
    /// LAST, so a keystroke a focused `FilterBar` turns into sink text —
    /// and a cursor move the delegation made — are reflected in the model
    /// of the SAME event.
    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        let before = self.win.state().size;
        // A divider nudge the keyboard-resize broker applied AFTER the
        // previous dispatch shows up here (the mouse-drag case is caught
        // by the same check below the delegation).
        if self.split_moved() {
            self.reapply_layout();
            self.dirty = true; // the layout changed: re-push the models
        }
        self.window_keys(ev, ctx);
        self.sync_from_core();
        // --- Mouse wheel: hand it to the FOCUSED pane's scroll bar. The
        // default routing would fan it to every child in reverse insertion
        // order and let `tree_bar` (inserted last) win it regardless of focus,
        // and the list viewer's cursor write-back would then stall the bar on
        // its first step — [`wheel_scrolls_bar`] documents both, and does the
        // localization and the re-assert that beats the write-back.
        if let Event::MouseWheel(_) = ev {
            let bar = if self.is_focused(self.tree_table) {
                self.tree_bar
            } else {
                self.layer_bar
            };
            if crate::widgets::wheel_scrolls_bar(&mut self.win, bar, ev, ctx) {
                return;
            }
        }
        // Windows ▸ Full screen: this window owns the inner `win` directly.
        let full = crate::app::take_fullscreen(ev, &mut self.win, ctx);
        if !full {
            self.win.handle_event(ev, ctx);
        }
        // The layer table's cursor owns `selected` (arrow keys and clicks
        // both land here through the delegation above), keyed by row id so
        // sorting and the layer filter don't shift it. Bounded by the
        // current analysis; the `sync_from_core` clamp stays the floor for
        // a shrunk layer list. Then the redraw below rebuilds the tree for
        // that layer.
        if let Some(i) = self.focused_layer()
            && i != self.selected
            && i < self.analysis.as_ref().map_or(0, |a| a.layers.len())
        {
            self.selected = i;
            self.dirty = true;
        }
        // Zoom / fullscreen re-bound the inner window's group directly,
        // bypassing this window's `change_bounds` (the Explorer's
        // reconcile): keep the child rects following the new client area.
        // The full-screen toggle also runs it when only the BORDER flipped,
        // and a dragged divider re-derives the middle rects the same way.
        if full || self.win.state().size != before || self.split_moved() {
            self.reapply_layout();
            self.win.on_bounds_changed(ctx);
            self.dirty = true; // the layout changed: re-push the models
        }
        // The gate (last, after everything that could mutate state): the
        // `FilterBar` children wrote their sinks inside `win.handle_event`
        // above, so the needles are compared HERE against the last
        // RENDERED ones. `redraw` itself clears the flag and records them.
        let needle_dirty = *self.layer_filter_sink.borrow() != self.last_layer_needle
            || *self.tree_filter_sink.borrow() != self.last_tree_needle;
        if self.dirty || needle_dirty {
            self.redraw(ctx);
        }
    }
}

#[cfg(test)]
mod tests;
