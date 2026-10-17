//! The Layers window: the image's layers on the left, the selected layer's
//! colour-coded change tree on the right, and the efficiency details at the
//! bottom. Mirrors the Explorer's window shape (two `Table`s over
//! `RowModel`s, a `Drop`-cleared reopen flag) but is a separate window —
//! the container-based Image Explorer is untouched.
use crate::core::{AppCore, TaskReq};
use crate::domain::layers::{Analysis, Compare, Comparer, DiffType, Tree};
use crate::widgets::resource_table::{RowModel, TableRow, to_spec};
use crate::widgets::{Align, Column, FilterBar, SharedLine};
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::mpsc;
use tv::event::Event;
use tv::widgets::{SelectionMode, Table};
use tv::{Context, HelpCtx, Rect, Role, View, ViewState, Window, delegate};

/// Help context while a Layers window is focused.
pub const HELP_CTX: HelpCtx = HelpCtx::custom("tvdocker.layers");

/// Window-menu number for the Layers window (1–6 resources, 7 Details,
/// 8 Logs, 9 Shell, 10 Explorer, 11 Diagnostics).
pub const WINDOW_NUMBER: i16 = 12;

/// The persisted column-layout key for the layer table.
const LAYER_TABLE_KEY: &str = "layers.layers";
/// The persisted column-layout key for the change tree.
const TREE_TABLE_KEY: &str = "layers.tree";

fn layer_columns() -> Vec<Column> {
    vec![
        Column::new("CMP", 3, Align::Left),
        Column::new("SIZE", 10, Align::Right),
        Column::new("COMMAND", 28, Align::Left),
    ]
}

fn tree_columns() -> Vec<Column> {
    vec![
        Column::new("PATH", 30, Align::Left),
        Column::new("CHANGE", 2, Align::Left),
        Column::new("SIZE", 10, Align::Right),
        Column::new("MODE", 8, Align::Left),
    ]
}

/// Child rectangles for a Layers window of `size` (frame included): a
/// details line, two filter bars, the two tables over the middle, and the
/// inefficient-paths strip above the bottom border.
fn layers_rects(size: tv::Point) -> LayersRects {
    let right = (size.x - 1).max(1); // exclusive client right (frame = size.x-1)
    let bottom = (size.y - 1).max(1); // exclusive client bottom (frame = size.y-1)
    let mid = (size.x / 2).clamp(1, right); // the layer/tree split column
    let first_table = 3.min(bottom); // filters end at row 3 — no blank line (the explorer's formula)
    // `bottom - 3`: the strip needs its header AND two path rows (the
    // design's "list of inefficient paths"); `bottom - 2` left room for a
    // single path, clipping every entry past the first at any window size.
    let details_y = (bottom - 3).max(first_table);
    let last_table = details_y.min(bottom);
    LayersRects {
        summary: Rect::new(1, 1, right, 2.min(bottom)),
        layer_filter: Rect::new(1, 2.min(bottom), mid - 1, 3.min(bottom)),
        tree_filter: Rect::new(mid + 1, 2.min(bottom), right, 3.min(bottom)),
        layer_table: Rect::new(1, first_table, mid - 1, last_table),
        tree_table: Rect::new(mid + 1, first_table, right, last_table),
        inefficient: Rect::new(1, details_y, right, bottom.max(details_y)),
    }
}

/// The [`layers_rects`] result — one rect per child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayersRects {
    summary: Rect,
    layer_filter: Rect,
    tree_filter: Rect,
    layer_table: Rect,
    tree_table: Rect,
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
    layer_table: tv::ViewId,
    tree_table: tv::ViewId,
    inefficient_table: tv::ViewId,
    layer_filter: tv::ViewId,
    tree_filter: tv::ViewId,
    summary_id: tv::ViewId,
    summary: Rc<RefCell<String>>,
    /// The selected layer index (the left table's focused row).
    selected: usize,
    /// Which comparison the change tree shows (rule 5).
    mode: Compare,
    comparer: Comparer,
    /// The flattened rows of the change tree, in DFS order.
    tree_rows: Vec<TreeRow>,
    /// The cached analysis (`None` = still loading). Like the Explorer's
    /// `Panel::entries`, the window holds the data and only pushes it into
    /// the tables where it has a `Context` (the `redraw_texts` pattern).
    analysis: Option<Analysis>,
    /// The last `layers_rev` this window mapped (`None` until the first
    /// sync, so a freshly built window always maps whatever the core has —
    /// even a directly-seeded test core that never bumped the counter).
    last_rev: Option<u64>,
}

fn make_table(rect: Rect, key: &str, columns: Vec<Column>, rows: Vec<TableRow>) -> Table {
    let specs = columns.iter().map(to_spec).collect();
    let mut table = Table::new(rect, specs, None)
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

/// The layer-table rows: a compare marker, the size and the command.
fn layer_rows(analysis: &Analysis, selected: usize) -> Vec<TableRow> {
    analysis
        .layers
        .iter()
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

/// `TreeRow`s as `TableRow`s: an indent-by-depth name, the change letter,
/// the size and the mode, tinted by [`role_for`]. Size and mode come from
/// `tree` — the comparison tree the rows were flattened from, not the
/// selected layer's tar — and `compare` back-fills a `Removed` node with
/// its `Entry`, so a removed row still shows what it used to be.
pub fn tree_rows_for(rows: &[TreeRow], tree: &Tree) -> Vec<TableRow> {
    rows.iter()
        .map(|r| {
            let entry = tree.get(&r.path).and_then(|id| tree.node(id).entry.clone());
            let size = entry
                .as_ref()
                .map(|e| crate::views::resource::format_size(e.size as i64))
                .unwrap_or_default();
            let mode = entry
                .as_ref()
                .map(|e| format!("{:o}", e.mode))
                .unwrap_or_default();
            let indent = " ".repeat(r.depth);
            let name = r.path.rsplit('/').next().unwrap_or(&r.path).to_string();
            TableRow::new(
                r.path.clone(),
                vec![
                    format!("{indent}{name}"),
                    letter_for(r.diff).to_string(),
                    size,
                    mode,
                ],
            )
            .with_opt_role(role_for(r.diff))
        })
        .collect()
}

/// Build one window. The initial analysis request is sent here — build
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
    let r = layers_rects(size);

    let summary = Rc::new(RefCell::new(String::new()));
    let layer_sink = Rc::new(RefCell::new(String::new()));
    let tree_sink = Rc::new(RefCell::new(String::new()));

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
    let layer_table = win.insert_child(Box::new(make_table(
        r.layer_table,
        LAYER_TABLE_KEY,
        layer_columns(),
        Vec::new(),
    )));
    let tree_table = win.insert_child(Box::new(make_table(
        r.tree_table,
        TREE_TABLE_KEY,
        tree_columns(),
        Vec::new(),
    )));
    let inefficient_table = win.insert_child(Box::new(make_table(
        r.inefficient,
        "layers.inefficient",
        vec![
            Column::new("COUNT", 6, Align::Right),
            Column::new("WASTED", 10, Align::Right),
            Column::new("PATH", 40, Align::Left),
        ],
        Vec::new(),
    )));

    let mut window = LayersWindow {
        win,
        core,
        task_tx,
        key,
        open,
        layer_table,
        tree_table,
        inefficient_table,
        layer_filter,
        tree_filter,
        summary_id,
        summary,
        selected: 0,
        mode: Compare::Single,
        comparer: Comparer::new(),
        tree_rows: Vec::new(),
        analysis: None,
        last_rev: None,
    };
    window.sync_from_core();
    // The initial analysis request (the Explorer's build-time request
    // precedent): the opener routes the command only, so the window asks
    // the worker itself — keyed AND image-typed by the image id.
    let _ = window.task_tx.send(TaskReq::AnalyzeImageLayers {
        key: window.key.clone(),
        image: window.key.clone(),
    });
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
    /// stale-response discipline, keyed on `layers_rev`. No `Context` here:
    /// [`redraw`] pushes the cached data into the tables.
    fn sync_from_core(&mut self) {
        let core = self.core.borrow();
        // Unchanged since the last map: nothing to do. `last_rev` is `None`
        // until the FIRST sync, so a freshly built window always maps —
        // even when a test seeds the slot without bumping `layers_rev`.
        if self.last_rev == Some(core.layers_rev) {
            return;
        }
        self.last_rev = Some(core.layers_rev);
        // Clone only when the revision actually moved: the analysis owns
        // full per-layer file trees.
        let analysis = core.image_layers.get(&self.key).cloned().flatten();
        let progress = core.layers_progress.get(&self.key).copied().unwrap_or(0);
        drop(core);
        match analysis {
            Some(a) => {
                self.selected = self.selected.min(a.layers.len().saturating_sub(1));
                *self.summary.borrow_mut() = format!(
                    " Total {}   Efficiency {:.0}%   Potential wasted space {}",
                    crate::views::resource::format_size(
                        a.layers.iter().map(|l| l.size as i64).sum()
                    ),
                    a.efficiency * 100.0,
                    crate::views::resource::format_size(a.wasted_bytes as i64),
                );
                self.analysis = Some(a);
            }
            None => {
                *self.summary.borrow_mut() = format!(
                    " Analyzing {} ({})…",
                    crate::views::resource::format_size(progress as i64),
                    self.key
                );
                self.analysis = None;
            }
        }
    }

    /// Push the cached analysis into the three tables (needs a `Context`, so
    /// it runs from `handle_event` — the `redraw_texts` pattern).
    fn redraw(&mut self, ctx: &mut Context) {
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
        let rows = layer_rows(&a, self.selected);
        if let Some(t) = self.table_mut(self.layer_table) {
            t.set_model(Box::new(RowModel::new(rows)), ctx);
        }
        // `Comparer::tree` borrows self mutably, so flatten into a local
        // before touching the table.
        let tree = self
            .comparer
            .tree(&a.layers, self.mode, self.selected)
            .clone();
        self.tree_rows = flatten(&tree);
        let rows = tree_rows_for(&self.tree_rows, &tree);
        if let Some(t) = self.table_mut(self.tree_table) {
            t.set_model(Box::new(RowModel::new(rows)), ctx);
        }
        if let Some(t) = self.table_mut(self.inefficient_table) {
            let rows: Vec<TableRow> = a
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
                .collect();
            t.set_model(Box::new(RowModel::new(rows)), ctx);
        }
    }

    /// Re-derive the child rects for the CURRENT window size and hand each
    /// child its new bounds (the Explorer's `reapply_layout`): without it
    /// the children keep their BUILD-time rects and freeze on resize.
    fn reapply_layout(&mut self) {
        let r = layers_rects(self.win.state().size);
        for (id, rect) in [
            (self.summary_id, r.summary),
            (self.layer_filter, r.layer_filter),
            (self.tree_filter, r.tree_filter),
            (self.layer_table, r.layer_table),
            (self.tree_table, r.tree_table),
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

    /// Every event: first map a fresh analysis, then push the cached rows
    /// into the tables (the Explorer's `sync_from_core` + `redraw_texts`).
    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        let before = self.win.state().size;
        self.sync_from_core();
        self.redraw(ctx);
        self.win.handle_event(ev, ctx);
        // Zoom / fullscreen re-bound the inner window's group directly,
        // bypassing this window's `change_bounds` (the Explorer's
        // reconcile): keep the child rects following the new client area.
        if self.win.state().size != before {
            self.reapply_layout();
            self.win.on_bounds_changed(ctx);
        }
    }
}

#[cfg(test)]
mod tests;
