//! The Explorer window: host fs left,
//! container/volume right, both panels `Table`s over the vendored
//! explorer domain. The window owns a task-worker sender (the
//! StartShell precedent) and applies only path-matching listings.
//!
//! Since the tvision-rs 0.17 migration both panels are upstream
//! [`tv::widgets::Table`]s built over a
//! [`RowModel`](crate::widgets::resource_table::RowModel). Keyboard and
//! mouse routing follow the FOCUS (which table holds the focused child)
//! instead of a window-owned `active_remote` flag, the inactive panel dims
//! automatically (the upstream body draws `Role::ListInactive`), and the
//! double-click is the framework's (`m.flags.double_click`, synthesized by
//! the pump) rather than a window-counted timer.

use crate::core::{AppCore, ExplorerTarget, TaskReq};
use crate::domain::config::{ColumnLayoutCfg, TvdockerConfig};
use crate::domain::docker::explorer::ExplorerEntry;
use crate::widgets::resource_table::{RowModel, TableRow, to_spec};
use crate::widgets::{Align, Column, FilterBar, SharedLine};
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::mpsc;
use tv::event::{Event, Key};
use tv::menu::{Menu, popup_menu};
use tv::widgets::{ScrollBar, SelectionMode, SortDir, Table};
use tv::{Command, Context, DrawCtx, FieldValue, Rect, Role, View, ViewState, Window, delegate};

/// Help context while an Explorer window is focused .
pub const HELP_CTX: tv::HelpCtx = tv::HelpCtx::custom("tvdocker.explorer");

/// Window-menu number for Explorer (7/8/9 = Details/Logs/Shell).
pub const WINDOW_NUMBER: i16 = 10;

/// The persisted column-layout key for the host panel's table
/// (`[column_layouts.explorer.host]`).
const HOST_TABLE_KEY: &str = "explorer.host";
/// The persisted column-layout key for the remote (container/volume) panel.
const REMOTE_TABLE_KEY: &str = "explorer.remote";

/// The explorer window's self-commands (keys post them; so do its menus
/// and modals). The window handles them itself — no desktop hop.
pub const CMD_EXP_REFRESH: Command = Command::custom("tvdocker.exp_refresh");
pub const CMD_EXP_DELETE: Command = Command::custom("tvdocker.exp_delete");
pub const CMD_EXP_DELETE_CONFIRMED: Command = Command::custom("tvdocker.exp_delete_confirmed");
pub const CMD_EXP_PROMPT: Command = Command::custom("tvdocker.exp_prompt");
pub const CMD_EXP_PROMPT_OK: Command = Command::custom("tvdocker.exp_prompt_ok");
pub const CMD_EXP_COPY: Command = Command::custom("tvdocker.exp_copy");
/// Menu-specific prompt openers — a menu item carries ONE command, so the
/// prompt kind rides on the command (a shared `CMD_EXP_PROMPT` could not
/// tell the three menu prompts apart).
pub const CMD_EXP_NEW_DIR: Command = Command::custom("tvdocker.exp_new_dir");
pub const CMD_EXP_NEW_FILE: Command = Command::custom("tvdocker.exp_new_file");
pub const CMD_EXP_RENAME: Command = Command::custom("tvdocker.exp_rename");
pub const CMD_EXP_GOTO: Command = Command::custom("tvdocker.exp_goto");

/// Which prompt a `CMD_EXP_PROMPT_OK` completes.
#[derive(Clone, Copy)]
enum PromptKind {
    NewDir,
    NewFile,
    Rename,
    Goto,
}

/// A 50×7 dialog centred in the owner (the `open_shell_config` bounds
/// precedent, sized for the one-field `InputDialog`).
fn prompt_dialog_rect(ctx: &Context) -> Rect {
    let size = ctx.owner_size();
    let w = 50.min(size.x);
    let h = 7.min(size.y);
    let x = (size.x - w) / 2;
    let y = (size.y - h) / 2;
    Rect::new(x, y, x + w, y + h)
}

/// Columns per panel (ruling 7): NAME/SIZE/MODIFIED fit the half-width.
fn columns() -> Vec<Column> {
    vec![
        Column::new("NAME", 16, Align::Left),
        Column::new("SIZE", 8, Align::Right),
        Column::new("MODIFIED", 12, Align::Left),
    ]
}

/// Child rectangles for an Explorer window of `size` (frame included):
/// breadcrumb on row 0, the two filter bars on row 1, the two tables over
/// rows 2..h-1 split at the middle column, the hint on row h-1.
///
/// These are DERIVED from the window size — never hardcoded — and are
/// re-applied whenever the size changes, the `pane_rects` rule this window
/// broke (its children froze at build time and mangled on resize).
/// Degenerate sizes yield empty-but-never-panicking rects.
fn explorer_rects(size: tv::Point) -> ExplorerRects {
    let right = (size.x - 1).max(1); // exclusive client right (frame = size.x-1)
    let bottom = (size.y - 1).max(1); // exclusive client bottom (frame = size.y-1)
    let mid = (size.x / 2).clamp(1, right); // the host/remote split column
    // The window FRAME owns the outer rows: in the window's local frame
    // y=0 IS the top border and y=size.y-1 the bottom one, so every client
    // row starts at 1 and the last client row is `bottom - 1` (the
    // `pane_rects` rule). The hint takes that last row, so the tables stop
    // one row above it. Degenerate sizes stay empty-but-never-inverted.
    let first_table = 3.min(bottom);
    let hint_y = (bottom - 1).max(first_table);
    let last_table = hint_y.min(bottom);
    // One column per panel, taken from that panel's own table: the host bar
    // is the LAST column of the host table, the remote bar the last of the
    // remote one. The `.max(…)`/`.min(…)` pairs keep every rect ordered even
    // at degenerate widths (a 1- or 2-column window), where the table may
    // collapse to zero width but must never invert.
    let host_body_right = (mid - 2).max(1);
    let host_bar_right = (mid - 1).max(host_body_right);
    let remote_body_right = (right - 1).max(mid + 1).min(right);
    let remote_bar_right = right.max(remote_body_right);
    ExplorerRects {
        breadcrumb_host: Rect::new(1, 1, mid - 1, 2.min(bottom)),
        breadcrumb_remote: Rect::new(mid + 1, 1, right, 2.min(bottom)),
        host_filter: Rect::new(1, 2.min(bottom), mid - 1, 3.min(bottom)),
        remote_filter: Rect::new(mid + 1, 2.min(bottom), right, 3.min(bottom)),
        host_table: Rect::new(1, first_table, host_body_right, last_table),
        host_bar: Rect::new(host_body_right, first_table, host_bar_right, last_table),
        remote_table: Rect::new(mid + 1, first_table, remote_body_right, last_table),
        remote_bar: Rect::new(remote_body_right, first_table, remote_bar_right, last_table),
        hint: Rect::new(1, hint_y, right, bottom.max(hint_y)),
    }
}

/// The [`explorer_rects`] result — one rect per child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExplorerRects {
    breadcrumb_host: Rect,
    breadcrumb_remote: Rect,
    host_filter: Rect,
    remote_filter: Rect,
    host_table: Rect,
    /// The host table's vertical scroll bar (its right-most column).
    host_bar: Rect,
    remote_table: Rect,
    /// The remote table's vertical scroll bar (its right-most column).
    remote_bar: Rect,
    hint: Rect,
}

/// One panel's view half: table + its scroll bar's and filter bar's ids.
struct Panel {
    table: tv::ViewId,
    /// The table's vertical scroll bar, in its right-most column.
    bar: tv::ViewId,
    filter: tv::ViewId,
    path: String,
    entries: Vec<ExplorerEntry>,
    last_rev: u64,
    /// The rows last pushed to the table — the gate for
    /// [`ExplorerWindow::push_rows`]. `TableRow: PartialEq`.
    last_rows: Vec<TableRow>,
    /// The `..` parent row exists for every path but "/".
    show_parent: bool,
}

/// Build the rows for one panel — the `..` row first (view-level, the
/// domain never emits it), dirs already sorted by the domain.
fn rows_for(entries: &[ExplorerEntry], show_parent: bool) -> Vec<TableRow> {
    let mut rows = Vec::with_capacity(entries.len() + 1);
    if show_parent {
        rows.push(TableRow::new(
            "..".to_string(),
            vec!["../".into(), String::new(), String::new()],
        ));
    }
    for e in entries {
        let name = if e.is_dir {
            format!("{}/", e.name)
        } else {
            e.name.clone()
        };
        let size = if e.is_dir {
            String::new()
        } else {
            crate::views::resource::format_size(e.size)
        };
        rows.push(TableRow::new(
            e.name.clone(),
            vec![name, size, e.modified.clone()],
        ));
    }
    rows
}

/// Build one panel's table: a multi-select table over `rows`, its group
/// marked selectable (so the window's focus pass treats it as a stop), wired
/// to `bar` (the vertical scroll bar in its right-most column), with the
/// persisted per-panel column layout restored when configured.
fn make_table(rect: Rect, table_key: &str, rows: Vec<TableRow>, bar: tv::ViewId) -> Table {
    let specs = columns().iter().map(to_spec).collect();
    let mut table = Table::new(rect, specs, Some(bar))
        .with_selection_mode(SelectionMode::Multi)
        .with_model(Box::new(RowModel::new(rows)));
    // The upstream `Table` is a `Group`: mark it selectable so focus can
    // land on it (its body is the selectable leaf inside).
    table.state_mut().options.selectable = true;
    if let Some(cfg) = TvdockerConfig::load().0.column_layout(table_key).cloned() {
        table.set_column_layout(&cfg.to_states());
    }
    table
}

/// The Explorer window: two `Table` panels over the host fs and the
/// container/volume, a shared hint row, and a task-worker sender for
/// list/copy/delete/create/rename requests.
pub struct ExplorerWindow {
    win: Window,
    core: Rc<RefCell<AppCore>>,
    task_tx: mpsc::UnboundedSender<TaskReq>,
    key: String,
    target: ExplorerTarget,
    open: Rc<RefCell<bool>>,
    host: Panel,
    remote: Panel,
    /// The breadcrumb path for each side (one tiny child view per column)
    /// + the hint text: all redrawn by those children.
    breadcrumb_host: Rc<RefCell<String>>,
    breadcrumb_remote: Rc<RefCell<String>>,
    hint: Rc<RefCell<String>>,
    /// Ids of the breadcrumb/hint `SharedLine`s, so the resize relayout can
    /// move them alongside the tables and filters.
    breadcrumb_host_id: tv::ViewId,
    breadcrumb_remote_id: tv::ViewId,
    hint_id: tv::ViewId,
    /// The prompt that is open (or about to be): create/rename/goto.
    pending_prompt: Option<PromptKind>,
    /// The pending Del's paths (the confirm round trip).
    pending_delete: Option<Vec<String>>,
    /// The answer of the last message box.
    modal_answer: Option<Command>,
    /// The last prompt's answered text (the `InputDialog` record, delivered
    /// via `set_modal_data`).
    prompt_value: String,
    /// Cached screen origin for right-click/Shift+F10 → `popup_menu` (the
    /// editor/`status_line` pattern).
    abs_origin: tv::Point,
}

impl ExplorerWindow {
    /// The home path for the host panel ($HOME, else "/").
    fn host_home() -> String {
        std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
    }

    /// The panel's table id (`remote` selects the container/volume side).
    fn panel_table(&self, remote: bool) -> tv::ViewId {
        if remote {
            self.remote.table
        } else {
            self.host.table
        }
    }

    /// Is child `id` the focused one? (tvision-rs has no `is_focused`, so
    /// this reads the delegated `focused` flag.)
    fn is_focused(&mut self, id: tv::ViewId) -> bool {
        self.win
            .child_mut(id)
            .is_some_and(|v| v.state().state.focused)
    }

    /// Which side receives the keys: the REMOTE panel is active when its
    /// table (not the host's) holds the focus. The inactive panel dims
    /// automatically (the upstream body draws `Role::ListInactive`).
    fn active_remote(&mut self) -> bool {
        self.is_focused(self.remote.table)
    }

    fn active(&mut self) -> &Panel {
        if self.active_remote() {
            &self.remote
        } else {
            &self.host
        }
    }

    fn active_mut(&mut self) -> &mut Panel {
        if self.active_remote() {
            &mut self.remote
        } else {
            &mut self.host
        }
    }

    /// The pane's table, downcast from the window group.
    fn table_mut(&mut self, id: tv::ViewId) -> Option<&mut Table> {
        self.win
            .child_mut(id)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<Table>())
    }

    /// Ask the pump to focus the HOST table once this window is wired in.
    ///
    /// A non-modal window's `View::reset_current` is **never called** —
    /// only `exec_view` calls it, and only for modals — so a window cannot
    /// establish its own internal currency. Left alone, the pump's
    /// `settle_currency` picks the last-inserted selectable child, which
    /// here is the REMOTE (usually empty) table: every Down/Enter then
    /// lands on the wrong panel. The opener requests the focus instead;
    /// the request is deferred and resolved by `find_mut`, so the id only
    /// has to be live when the pump drains (right after the insert).
    pub fn request_initial_focus(&mut self, ctx: &mut Context) {
        ctx.request_focus(self.host.table);
    }

    /// Apply a fresh listing if it matches the panel's current path —
    /// the stale-response drop (Review Focus #1).
    fn sync_from_core(&mut self) {
        let (host_slot, remote_slot) = {
            let core = self.core.borrow();
            (
                core.explorer.get(&format!("{}:h", self.key)).cloned(),
                core.explorer.get(&format!("{}:r", self.key)).cloned(),
            )
        };
        for (panel, slot) in [(&mut self.host, host_slot), (&mut self.remote, remote_slot)] {
            if let Some(listing) = slot
                && listing.rev != panel.last_rev
            {
                panel.last_rev = listing.rev;
                if listing.path == panel.path {
                    panel.entries = listing.entries;
                    panel.show_parent = panel.path != "/";
                }
                // else: stale — the panel keeps its own state.
            }
        }
    }

    /// Push the table rows + breadcrumb/hint text after any change.
    fn redraw_texts(&mut self, ctx: &mut Context) {
        // Each pane shows its own absolute path, aligned with its column —
        // not one jammed-together string pinned to the left edge.
        *self.breadcrumb_host.borrow_mut() = format!(" {}", self.host.path);
        *self.breadcrumb_remote.borrow_mut() = format!(" {}", self.remote.path);
        let hint = |p: &Panel| {
            let n = p.entries.len();
            let parent = usize::from(p.show_parent);
            format!("{} items", n + parent)
        };
        *self.hint.borrow_mut() = format!(
            "{} · {} · Tab panels · / filter · Enter open · Backspace up · Del delete",
            hint(&self.host),
            hint(&self.remote)
        );
        let host_rows = rows_for(&self.host.entries, self.host.show_parent);
        let remote_rows = rows_for(&self.remote.entries, self.remote.show_parent);
        self.push_rows(false, host_rows, ctx);
        self.push_rows(true, remote_rows, ctx);
    }

    /// Push `rows` into panel `remote`'s table — but ONLY when they differ
    /// from what that table already holds.
    ///
    /// This gate is load-bearing, not an optimisation. [`Table::set_model`]
    /// publishes the model, and publishing calls
    /// `list_viewer::focus_item_num(focused)`, which re-anchors `top_item`
    /// on the CURSOR row and re-publishes that row index to the scroll bar.
    /// `redraw_texts` runs on EVERY event, so without the gate each one
    /// snapped the viewport back to the cursor — silently undoing any
    /// scroll the bar had just applied. That is why the scroll bars and the
    /// mouse wheel looked dead: only key scrolling survived, because it
    /// moves the cursor itself.
    fn push_rows(&mut self, remote: bool, rows: Vec<TableRow>, ctx: &mut Context) {
        {
            let panel = if remote {
                &mut self.remote
            } else {
                &mut self.host
            };
            if panel.last_rows == rows {
                return;
            }
            panel.last_rows = rows.clone();
        }
        let table_id = if remote {
            self.remote.table
        } else {
            self.host.table
        };
        if let Some(t) = self.table_mut(table_id) {
            t.set_model(Box::new(RowModel::new(rows)), ctx);
        }
    }

    /// Re-derive the child rects for the CURRENT window size and hand each
    /// child its new bounds. Called from [`View::change_bounds`] (every
    /// deferred/owner-driven resize) and from the event-time reconcile that
    /// covers zoom/fullscreen (which re-bound the inner window's group
    /// directly, bypassing this window's `change_bounds`).
    ///
    /// Without it the children keep their BUILD-time rects: `Group::
    /// change_bounds` only applies grow-mode deltas, and these children use
    /// the default (fixed) grow mode — so the panels froze at their original
    /// coordinates and overlapped the moved frame.
    fn reapply_layout(&mut self) {
        let r = explorer_rects(self.win.state().size);
        for (id, rect) in [
            (self.breadcrumb_host_id, r.breadcrumb_host),
            (self.breadcrumb_remote_id, r.breadcrumb_remote),
            (self.host.filter, r.host_filter),
            (self.remote.filter, r.remote_filter),
            (self.host.bar, r.host_bar),
            (self.remote.bar, r.remote_bar),
            (self.host.table, r.host_table),
            (self.remote.table, r.remote_table),
            (self.hint_id, r.hint),
        ] {
            if let Some(v) = self.win.child_mut(id) {
                v.change_bounds(rect);
            }
        }
    }

    /// The panel (host/remote) whose table bounds contain window-local `pos`.
    fn panel_at(&mut self, pos: tv::Point) -> Option<bool> {
        for remote in [false, true] {
            let id = self.panel_table(remote);
            if let Some(v) = self.win.child_mut(id)
                && v.state().get_bounds().contains(pos)
            {
                return Some(remote);
            }
        }
        None
    }

    /// Focus the panel `remote` and the row under window-local `pos`: the
    /// table is focused through the window group, then a synthetic left
    /// press is delivered to the table through a throwaway `Context` so the
    /// body focuses the clicked row exactly as upstream would. The scratch
    /// queue, timers and — decisively — the mouse-hold capture the press
    /// arms are all discarded: a left/right click is not a drag, and leaving
    /// that hold capture live would swallow the NEXT event (for a bare
    /// MouseDown, the following key) and, for a right-click, the command the
    /// context menu posts on release.
    fn focus_row_at(&mut self, remote: bool, pos: tv::Point, ctx: &mut Context) {
        let table_id = self.panel_table(remote);
        self.win.focus_descendant(table_id, ctx);
        let Some(t) = self.table_mut(table_id) else {
            return;
        };
        let local = pos - t.state().get_bounds().a;
        let mut out = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        let mut scratch = Context::new(&mut out, &mut timers, 0, &mut deferred);
        let mut ev = Event::MouseDown(tv::MouseEvent {
            position: local,
            buttons: tv::event::MouseButtons {
                left: true,
                ..tv::event::MouseButtons::default()
            },
            ..tv::MouseEvent::default()
        });
        t.handle_event(&mut ev, &mut scratch);
    }

    /// Cycle the sort column on ←/→ (backward walks the other way and lands
    /// DESCENDING, as the old table did), or flip the direction on
    /// Shift+←/→. The upstream `Table` only exposes `TABLE_SORT_NEXT`
    /// (forward, ascending) and `TABLE_SORT_FLIP`, so the window keeps
    /// tvdocker's documented cycle and drives [`Table::sort_by`] itself.
    fn cycle_sort(&mut self, remote: bool, forward: bool, flip: bool, ctx: &mut Context) {
        let id = self.panel_table(remote);
        let Some(t) = self.table_mut(id) else {
            return;
        };
        let layout = t.column_layout();
        let n = layout.len();
        if n == 0 {
            return;
        }
        let mut visible = vec![false; n];
        for s in &layout {
            if s.column < n {
                visible[s.column] = !s.hidden;
            }
        }
        let current = t.sort_spec();
        let (col, dir) = if flip {
            match current {
                Some((c, d)) => (c, d.flip()),
                None => match (0..n).find(|&i| visible[i]) {
                    Some(c) => (c, SortDir::Asc),
                    None => return,
                },
            }
        } else {
            let step: i64 = if forward { 1 } else { -1 };
            // No sort yet: start one past the first / last column so the first
            // step lands ON it.
            let start = current.map_or(if forward { -1 } else { n as i64 }, |(c, _)| c as i64);
            let mut found = None;
            let mut probe = start;
            for _ in 0..n {
                probe = (probe + step).rem_euclid(n as i64);
                let i = probe as usize;
                if visible[i] {
                    found = Some(i);
                    break;
                }
            }
            match found {
                Some(c) => (c, if forward { SortDir::Asc } else { SortDir::Desc }),
                None => return,
            }
        };
        t.sort_by(col, dir, ctx);
    }
}

/// Build one window. The two initial listings are requested
/// here — build cannot fail, and an empty panel simply renders "0 items".
pub fn build(
    core: Rc<RefCell<AppCore>>,
    bounds: Rect,
    key: String,
    target: ExplorerTarget,
    label: String,
    open: Rc<RefCell<bool>>,
    task_tx: mpsc::UnboundedSender<TaskReq>,
) -> ExplorerWindow {
    let title = format!("Explorer — {label}");
    let mut win = Window::new(bounds, Some(title), WINDOW_NUMBER);
    let size = win.state().size;
    let r = explorer_rects(size);

    let host_sink = Rc::new(RefCell::new(String::new()));
    let remote_sink = Rc::new(RefCell::new(String::new()));
    let breadcrumb_host = Rc::new(RefCell::new(String::new()));
    let breadcrumb_remote = Rc::new(RefCell::new(String::new()));
    let hint = Rc::new(RefCell::new(String::new()));

    // Row 1: the breadcrumb — one path per column, above its pane (row 0 is
    // the frame's top border). Row 2: the two filters. Rows 3..h-2: the
    // tables. Row h-2: the hint (row h-1 is the frame's bottom border).
    let breadcrumb_host_id = win.insert_child(Box::new(SharedLine::new(
        r.breadcrumb_host,
        Rc::clone(&breadcrumb_host),
        Role::Info,
    )));
    let breadcrumb_remote_id = win.insert_child(Box::new(SharedLine::new(
        r.breadcrumb_remote,
        Rc::clone(&breadcrumb_remote),
        Role::Info,
    )));
    let host_filter = win.insert_child(Box::new(FilterBar::new(
        r.host_filter,
        Rc::clone(&host_sink),
        "host filter (/)",
    )));
    let remote_filter = win.insert_child(Box::new(FilterBar::new(
        r.remote_filter,
        Rc::clone(&remote_sink),
        "remote filter (/)",
    )));

    let host_path = ExplorerWindow::host_home();
    // The initial models: the host's `..` parent row (unless at "/") and no
    // remote rows. A listing replaces them on the first event.
    let host_rows = rows_for(&[], host_path != "/");
    let remote_rows = rows_for(&[], false);
    // Bars FIRST (the `build_scroll_window` / layers pattern): the table
    // wires itself to the bar's id at construction and publishes its row
    // range to it on every model push.
    let host_bar = win.insert_child(Box::new(ScrollBar::new(r.host_bar)));
    let host_table = win.insert_child(Box::new(make_table(
        r.host_table,
        HOST_TABLE_KEY,
        host_rows,
        host_bar,
    )));
    let remote_bar = win.insert_child(Box::new(ScrollBar::new(r.remote_bar)));
    let remote_table = win.insert_child(Box::new(make_table(
        r.remote_table,
        REMOTE_TABLE_KEY,
        remote_rows,
        remote_bar,
    )));
    let hint_id = win.insert_child(Box::new(SharedLine::new(
        r.hint,
        Rc::clone(&hint),
        Role::Info,
    )));

    let window = ExplorerWindow {
        win,
        core,
        task_tx,
        key: key.clone(),
        target,
        open,
        host: Panel {
            table: host_table,
            bar: host_bar,
            filter: host_filter,
            path: host_path.clone(),
            entries: Vec::new(),
            last_rev: 0,
            show_parent: host_path != "/",
            last_rows: Vec::new(),
        },
        remote: Panel {
            table: remote_table,
            bar: remote_bar,
            filter: remote_filter,
            path: "/".to_string(),
            entries: Vec::new(),
            last_rev: 0,
            show_parent: false,
            last_rows: Vec::new(),
        },
        breadcrumb_host,
        breadcrumb_remote,
        hint,
        breadcrumb_host_id,
        breadcrumb_remote_id,
        hint_id,
        pending_prompt: None,
        pending_delete: None,
        modal_answer: None,
        prompt_value: String::new(),
        abs_origin: tv::Point::new(0, 0),
    };
    // The two initial listings (ruling: build-time request).
    let _ = window.task_tx.send(TaskReq::ExplorerList {
        key,
        target: ExplorerTarget::Host,
        path: host_path,
    });
    let target = window.target.clone();
    let key = window.key.clone();
    let _ = window.task_tx.send(TaskReq::ExplorerList {
        key,
        target,
        path: "/".to_string(),
    });
    window
}

#[delegate(to = win)]
impl View for ExplorerWindow {
    fn state(&self) -> &ViewState {
        self.win.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.win.state_mut()
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.abs_origin = ctx.origin(); // right-click → global coords
        self.win.draw(ctx);
    }

    /// Re-derive the child rects whenever the window size changes (zoom /
    /// Size-Move drag / terminal resize — every path now goes through this
    /// method). Without it the children keep their BUILD-time rects.
    fn change_bounds(&mut self, bounds: Rect) {
        self.win.change_bounds(bounds);
        self.reapply_layout();
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        let before = self.win.state().size;
        self.sync_from_core();
        // --- The upstream Table arranged its columns (resize / move / show /
        // hide): persist the new layout for the panel's key. The broadcast
        // carries the table's own id as `source`.
        if let Event::Broadcast { command, source } = ev
            && *command == Command::TABLE_COLUMNS_CHANGED
            && (*source == Some(self.host.table) || *source == Some(self.remote.table))
        {
            let (table_id, table_key) = if *source == Some(self.host.table) {
                (self.host.table, HOST_TABLE_KEY)
            } else {
                (self.remote.table, REMOTE_TABLE_KEY)
            };
            let states = self
                .table_mut(table_id)
                .map(|t| t.column_layout())
                .unwrap_or_default();
            let _ = TvdockerConfig::set_column_layout_in_file(
                &TvdockerConfig::path(),
                table_key,
                &ColumnLayoutCfg::from_states(&states),
            );
            ev.clear();
            return;
        }
        // --- Tab: exactly two stops (Ruling 3). The window intercepts
        // before the group's default traversal (which would also stop at
        // the selectable FilterBars). tv has no `BackTab` variant —
        // Shift+Tab is `Key::Tab` + shift — and the brief's tests use
        // plain Tab only. Routing follows the FOCUS: a focused filter
        // escapes back to its own table, otherwise the focus toggles
        // between the two tables.
        if let Event::KeyDown(ke) = ev
            && ke.key == Key::Tab
        {
            let target = if self.is_focused(self.host.filter) {
                self.host.table
            } else if self.is_focused(self.remote.filter) {
                self.remote.table
            } else if self.is_focused(self.remote.table) {
                self.host.table
            } else {
                self.remote.table
            };
            self.win.focus_descendant(target, ctx);
            self.redraw_texts(ctx);
            ev.clear();
            return;
        }
        // --- Esc: a focused filter escapes back to its table. When no
        // filter is focused, Esc falls through to the children.
        if let Event::KeyDown(ke) = ev
            && ke.key == Key::Esc
        {
            let target = if self.is_focused(self.host.filter) {
                Some(self.host.table)
            } else if self.is_focused(self.remote.filter) {
                Some(self.remote.table)
            } else {
                None
            };
            if let Some(table) = target {
                self.win.focus_descendant(table, ctx);
                ev.clear();
                return;
            }
        }
        // --- `/` focuses the active panel's filter .
        if let Event::KeyDown(ke) = ev
            && matches!(ke.key, Key::Char('/'))
        {
            let remote = self.active_remote();
            let filter = if remote {
                self.remote.filter
            } else {
                self.host.filter
            };
            self.win.focus_descendant(filter, ctx);
            ev.clear();
            return;
        }
        // --- Navigation : Enter descends, Backspace climbs.
        // Both bubble out of the Table (it consumes neither).
        if let Event::KeyDown(ke) = ev {
            match ke.key {
                Key::Enter => {
                    self.enter(ctx);
                    ev.clear();
                    return;
                }
                Key::Backspace => {
                    self.go_up(ctx);
                    ev.clear();
                    return;
                }
                _ => {}
            }
        }
        // --- Sort keys (←/→ cycle the column, Shift flips the direction):
        // the window owns them because the upstream `Table` command layer
        // only has forward-ascending and flip. Only when a table body holds
        // the focus (a filter keeps its own ←/→ caret movement).
        if let Event::KeyDown(ke) = ev
            && matches!(ke.key, Key::Left | Key::Right)
        {
            let remote = self.active_remote();
            if self.is_focused(self.panel_table(remote)) {
                self.cycle_sort(remote, ke.key == Key::Right, ke.modifiers.shift, ctx);
                ev.clear();
                return;
            }
        }
        // --- Shift+F10: the mouse-free context menu — the same popup a
        // right-click opens, anchored at the ACTIVE panel's cursor cell
        // (a right-click targets the CLICKED panel's row; the keyboard
        // follows the focus).
        if let Event::KeyDown(ke) = ev
            && ke.key == Key::F(10)
            && ke.modifiers.shift
        {
            if let Some(anchor) = self.context_anchor() {
                self.context_menu_at(anchor, ctx);
            }
            ev.clear();
            return;
        }
        // --- Self-commands (keys/menus/modals post them).
        if let Event::Command(cmd) = ev {
            if *cmd == CMD_EXP_REFRESH {
                self.relist_active();
                ev.clear();
                return;
            }
            if *cmd == CMD_EXP_COPY {
                self.copy_active();
                ev.clear();
                return;
            }
            if *cmd == CMD_EXP_DELETE && !self.read_only() {
                self.confirm_delete(ctx);
                ev.clear();
                return;
            }
            if *cmd == CMD_EXP_DELETE_CONFIRMED {
                self.finish_delete();
                ev.clear();
                return;
            }
            if *cmd == CMD_EXP_PROMPT {
                self.open_prompt(ctx);
                ev.clear();
                return;
            }
            if *cmd == CMD_EXP_PROMPT_OK {
                self.finish_prompt();
                ev.clear();
                return;
            }
            if *cmd == CMD_EXP_NEW_DIR && !self.read_only() {
                self.pending_prompt = Some(PromptKind::NewDir);
                self.open_prompt(ctx);
                ev.clear();
                return;
            }
            if *cmd == CMD_EXP_NEW_FILE && !self.read_only() {
                self.pending_prompt = Some(PromptKind::NewFile);
                self.open_prompt(ctx);
                ev.clear();
                return;
            }
            if *cmd == CMD_EXP_RENAME && !self.read_only() {
                self.pending_prompt = Some(PromptKind::Rename);
                self.open_prompt(ctx);
                ev.clear();
                return;
            }
            if *cmd == CMD_EXP_GOTO {
                self.pending_prompt = Some(PromptKind::Goto);
                self.open_prompt(ctx);
                ev.clear();
                return;
            }
        }
        // --- Printables + Del/F5 (NEVER status-bound; a
        // focused filter swallows them instead — correct UX).
        if let Event::KeyDown(ke) = ev
            && !self.filter_focused()
        {
            match (ke.key, ke.modifiers.ctrl) {
                (Key::Delete, _) if !self.read_only() => {
                    self.confirm_delete(ctx);
                    ev.clear();
                    return;
                }
                (Key::F(5), _) => {
                    self.relist_active();
                    ev.clear();
                    return;
                }
                (Key::Char('n'), false) if !self.read_only() => {
                    self.pending_prompt = Some(PromptKind::NewDir);
                    ctx.post(CMD_EXP_PROMPT);
                    ev.clear();
                    return;
                }
                (Key::Char('N'), false) if !self.read_only() => {
                    self.pending_prompt = Some(PromptKind::NewFile);
                    ctx.post(CMD_EXP_PROMPT);
                    ev.clear();
                    return;
                }
                (Key::Char('r'), false) if !self.read_only() => {
                    self.pending_prompt = Some(PromptKind::Rename);
                    ctx.post(CMD_EXP_PROMPT);
                    ev.clear();
                    return;
                }
                (Key::Char('p'), true) => {
                    self.pending_prompt = Some(PromptKind::Goto);
                    ctx.post(CMD_EXP_PROMPT);
                    ev.clear();
                    return;
                }
                (Key::Char('X'), false) | (Key::Char('c'), true) => {
                    let remote = self.active_remote();
                    if self.read_only() && !remote {
                        ev.clear();
                        return;
                    }
                    self.copy_active();
                    ev.clear();
                    return;
                }
                (Key::Char('R'), false) => {
                    self.relist_active();
                    ev.clear();
                    return;
                }
                (Key::Char('g'), false) => {
                    self.jump(false, ctx);
                    ev.clear();
                    return;
                }
                (Key::Char('G'), false) => {
                    self.jump(true, ctx);
                    ev.clear();
                    return;
                }
                _ => {}
            }
        }
        // --- Mouse: the ROUTING (not a window flag) decides which panel a
        // click belongs to. A left click on a panel's BODY focuses that panel
        // and the row under the pointer — delivered through a throwaway
        // `Context` by [`focus_row_at`], so the mouse-hold capture the body
        // arms on press is discarded instead of swallowing the NEXT event (a
        // bare MouseDown in the pump would otherwise hold keys until a
        // MouseUp that a click-only stream never sends). A press on the
        // table's HEADER row is the table's own (click-to-sort / resize /
        // move), so it is forwarded to the group. A DOUBLE-click
        // (`m.flags.double_click`, synthesized by the pump) OPENS the row —
        // the file-manager convention, the same request Enter makes.
        // Right-click opens the context menu (the ResourceWindow precedent).
        if let Event::MouseDown(m) = ev {
            let m = *m;
            if m.buttons.left
                && let Some(remote) = self.panel_at(m.position)
            {
                let table_id = self.panel_table(remote);
                // Table-local y == 0 is the header row; any lower row is body.
                let on_header = self
                    .win
                    .child_mut(table_id)
                    .is_some_and(|v| v.state().get_bounds().a.y == m.position.y);
                if !on_header {
                    self.focus_row_at(remote, m.position, ctx);
                    if m.flags.double_click {
                        self.enter(ctx); // climbs/descends; a no-op on files
                    }
                    ev.clear();
                    return;
                }
            }
            if m.buttons.right
                && !m.buttons.left
                && let Some(remote) = self.panel_at(m.position)
            {
                self.focus_row_at(remote, m.position, ctx);
                self.context_menu_at(m.position, ctx);
                ev.clear();
                return;
            }
        }
        // --- Mouse wheel: the group fans this out to EVERY child in reverse
        // insertion order and stops at the first one that consumes it, and
        // only the `ScrollBar`s handle the wheel. Since this window inserts
        // `host_bar` BEFORE `remote_bar`, the remote bar was reached first and
        // consumed every scroll — so the wheel always drove the right panel
        // and could never reach the host at all. Deliver it to the ACTIVE
        // panel's bar instead (the same focus-follows routing the keys use),
        // localized into the bar's frame exactly as the group's `deliver`
        // would have done, and always consume it so no sibling steals it.
        if let Event::MouseWheel(m) = ev {
            let m = *m;
            let bar = if self.active_remote() {
                self.remote.bar
            } else {
                self.host.bar
            };
            let mut local = Event::MouseWheel(m);
            let stepped = {
                let Some(v) = self.win.child_mut(bar) else {
                    ev.clear();
                    return;
                };
                if let Event::MouseWheel(lm) = &mut local {
                    lm.position -= v.state().get_bounds().a;
                }
                v.handle_event(&mut local, ctx);
                v.as_any_mut()
                    .and_then(|a| a.downcast_mut::<tv::widgets::ScrollBar>())
                    .map(|sb| sb.value)
            };
            if let Some(value) = stepped {
                // Re-assert the value the bar just chose, as a DEFERRED.
                //
                // The wheel made the BAR the source of truth, but the table's
                // cursor disagrees, and `list_viewer::focus_item` reconciles
                // by pushing the cursor row back into the bar — also deferred.
                // That write-back was queued a pass EARLIER, so in the deferred
                // drain it lands after ours and wins: the bar snapped back to
                // the cursor row, the sync then read the stale cursor, and the
                // panel never scrolled past it (a stable stall, which is why
                // only the first tick ever appeared to move anything). Queueing
                // our own write behind it makes ours the LAST write in the
                // drain, so the sync that follows reads OUR value and the
                // cursor catches up instead of fighting back.
                ctx.request_scroll_bar_params(bar, Some(value), None, None, None, None);
            }
            ev.clear();
            return;
        }
        self.redraw_texts(ctx);
        // Windows ▸ Full screen: this window owns the inner `win` directly,
        // so the toggle happens here (before anything else sees the event).
        let full = crate::app::take_fullscreen(ev, &mut self.win, ctx);
        if !full {
            self.win.handle_event(ev, ctx);
        }
        // Zoom / fullscreen re-bound the inner window's group directly
        // (`Window::maximize` -> `locate` -> `group.change_bounds`), which never
        // reaches this window's `change_bounds`; reconcile so the panels and
        // their breadcrumb/filter/hint children follow the new client area (and
        // the tables re-publish their scroll bars). The toggle runs the
        // reconcile even when only the BORDER flipped (size unchanged).
        if full || self.win.state().size != before {
            self.reapply_layout();
            self.win.on_bounds_changed(ctx);
        }
    }

    fn set_modal_answer(&mut self, cmd: Command) {
        self.modal_answer = Some(cmd);
    }

    /// The prompt dialog's record: its single text. The pump delivers this
    /// BEFORE the close command, so `finish_prompt` reads the answered text.
    fn set_modal_data(&mut self, data: FieldValue) {
        if let FieldValue::Text(t) = data {
            self.prompt_value = t;
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl ExplorerWindow {
    /// Enter: on the `..` row climb; on a dir row descend; files are
    /// no-ops (copy is the file action).
    fn enter(&mut self, ctx: &mut Context) {
        let remote = self.active_remote();
        let table_id = self.panel_table(remote);
        let cursor = self.table_mut(table_id).and_then(|t| t.focused_row());
        let Some(cursor) = cursor else {
            return;
        };
        let target = if remote {
            self.target.clone()
        } else {
            ExplorerTarget::Host
        };
        let (path, entries, show_parent) = {
            let p = self.active();
            (p.path.clone(), p.entries.clone(), p.path != "/")
        };
        // The `..` row sits in front when not at a root — climb instead.
        if show_parent && cursor == 0 {
            self.go_up(ctx);
            return;
        }
        let idx = cursor - usize::from(show_parent); // ".." occupies row 0
        let Some(e) = entries.get(idx) else { return };
        if !e.is_dir {
            return; // files: no-op (copy/delete carry the file actions)
        }
        let new_path = format!("{}/{}", path.trim_end_matches('/'), e.name);
        let panel = self.active_mut();
        panel.path = new_path.clone();
        panel.show_parent = true;
        let _ = self.task_tx.send(TaskReq::ExplorerList {
            key: self.key.clone(),
            target,
            path: new_path,
        });
        let _ = ctx;
    }

    /// Backspace / the `..` row: climb one directory.
    fn go_up(&mut self, ctx: &mut Context) {
        let remote = self.active_remote();
        let parent = {
            let p = self.active();
            if p.path == "/" {
                return;
            }
            let parent = std::path::Path::new(&p.path)
                .parent()
                .map(|q| q.to_string_lossy().to_string())
                .unwrap_or_else(|| "/".to_string());
            if parent.is_empty() {
                "/".to_string()
            } else {
                parent
            }
        };
        let target = if remote {
            self.target.clone()
        } else {
            ExplorerTarget::Host
        };
        let panel = self.active_mut();
        panel.path = parent.clone();
        panel.show_parent = parent != "/";
        let _ = self.task_tx.send(TaskReq::ExplorerList {
            key: self.key.clone(),
            target,
            path: parent,
        });
        let _ = ctx;
    }

    /// Is one of the filter bars focused? (gate for the printables)
    fn filter_focused(&mut self) -> bool {
        self.is_focused(self.host.filter) || self.is_focused(self.remote.filter)
    }

    /// Re-request the active panel's current listing (F5 / `R`).
    fn relist_active(&mut self) {
        let remote = self.active_remote();
        let target = if remote {
            self.target.clone()
        } else {
            ExplorerTarget::Host
        };
        let path = self.active().path.clone();
        let _ = self.task_tx.send(TaskReq::ExplorerList {
            key: self.key.clone(),
            target,
            path,
        });
    }

    /// g/G: jump the active table to the first/last row.
    fn jump(&mut self, bottom: bool, ctx: &mut Context) {
        let remote = self.active_remote();
        let table = self.panel_table(remote);
        let Some(t) = self.table_mut(table) else {
            return;
        };
        let key = {
            let order = t.row_order();
            let model = t.model();
            let row = if bottom {
                order.last().copied()
            } else {
                order.first().copied()
            };
            row.map(|r| model.row_key(r))
        };
        if let Some(key) = key {
            t.set_focused_key(&key, ctx);
        }
    }

    /// The active panel's selection-or-cursor paths (selection wins).
    fn targets(&mut self) -> Vec<String> {
        let remote = self.active_remote();
        let panel_dir = self.active().path.clone();
        let table_id = self.panel_table(remote);
        let Some(t) = self.table_mut(table_id) else {
            return Vec::new();
        };
        let selected: Vec<String> = t.selected_keys().into_iter().map(|k| k.0).collect();
        let ids = if selected.is_empty() {
            let model = t.model();
            t.focused_row()
                .map(|r| model.row_key(r).0)
                .into_iter()
                .collect()
        } else {
            selected
        };
        ids.into_iter()
            .filter(|n| n != "..")
            .map(|n| format!("{}/{}", panel_dir.trim_end_matches('/'), n))
            .collect()
    }

    /// X / Ctrl+C: copy the active panel's targets to the other side.
    /// The remote side is an image, so nothing in this window may write —
    /// neither into the image nor onto the host (image-explorer).
    fn read_only(&self) -> bool {
        matches!(self.target, ExplorerTarget::Image(_))
    }

    /// Where the keyboard route (Shift+F10) opens the menu: the ACTIVE
    /// panel's cursor cell — the point a right-click on that row would
    /// use. The caret is the only cursor the upstream Table exposes and
    /// it is hidden while a filter bar holds focus, so that case falls
    /// back to the table's top-left (the menu's ITEMS follow the active
    /// panel either way).
    fn context_anchor(&mut self) -> Option<tv::Point> {
        let remote = self.active_remote();
        let t = self.table_mut(self.panel_table(remote))?;
        let bounds = t.state().get_bounds();
        let cursor = t.cursor_request().unwrap_or(tv::Point::new(0, 1));
        Some(tv::Point::new(bounds.a.x + cursor.x, bounds.a.y + cursor.y))
    }

    /// The ACTIVE panel's context menu at window-local `anchor` — the
    /// right-click point, or the keyboard route's cursor cell.
    fn context_menu_at(&mut self, anchor: tv::Point, ctx: &mut Context) {
        let mut builder = Menu::builder();
        for (label, command) in self.context_menu_items() {
            builder = builder.command(label, command);
        }
        popup_menu(
            anchor + self.abs_origin,
            builder.build(),
            ctx.owner_size(),
            ctx,
        );
    }

    /// The context menu for the ACTIVE panel. An image window offers only
    /// copying out of the image plus refresh .
    fn context_menu_items(&mut self) -> Vec<(String, Command)> {
        if self.read_only() {
            let mut items = Vec::new();
            if self.active_remote() {
                items.push(("~c~opy to host".to_string(), CMD_EXP_COPY));
            }
            items.push(("~R~efresh".to_string(), CMD_EXP_REFRESH));
            return items;
        }
        vec![
            ("~c~opy to other side".to_string(), CMD_EXP_COPY),
            ("~r~ename…".to_string(), CMD_EXP_RENAME),
            ("~n~ew dir…".to_string(), CMD_EXP_NEW_DIR),
            ("new ~f~ile…".to_string(), CMD_EXP_NEW_FILE),
            ("~R~efresh".to_string(), CMD_EXP_REFRESH),
            ("~x~Delete…".to_string(), CMD_EXP_DELETE),
        ]
    }

    fn copy_active(&mut self) {
        // An image may only be copied OUT of .
        let remote = self.active_remote();
        if self.read_only() && !remote {
            return;
        }
        let items = self.targets();
        if items.is_empty() {
            return;
        }
        let (target, to_container, dst) = if remote {
            (self.target.clone(), false, self.host.path.clone())
        } else {
            (self.target.clone(), true, self.remote.path.clone())
        };
        let _ = self.task_tx.send(TaskReq::ExplorerCopy {
            key: self.key.clone(),
            target,
            to_container,
            items,
            dst,
        });
    }

    /// Del: the window-owned Yes/No confirm (scoped).
    fn confirm_delete(&mut self, ctx: &mut Context) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        self.pending_delete = Some(targets.clone());
        let text = if targets.len() == 1 {
            format!("Delete '{}'?", targets[0])
        } else {
            format!("Delete {} items?", targets.len())
        };
        let Some(requester) = self.state().id() else {
            return;
        };
        ctx.request_message_box(
            text,
            tv::MessageBoxKind::Confirmation,
            tv::MessageBoxButtons::yes_no(),
            Some(requester),
            Some(CMD_EXP_DELETE_CONFIRMED),
        );
    }

    fn finish_delete(&mut self) {
        let paths = self.pending_delete.take();
        if self.modal_answer.take() != Some(tv::Command::YES) {
            return;
        }
        let Some(paths) = paths else {
            return;
        };
        let _ = self.task_tx.send(TaskReq::ExplorerDelete {
            key: self.key.clone(),
            target: self.target.clone(),
            paths,
        });
    }

    /// The active table's focused row id (absent when the table has no rows).
    fn cursor_row_id(&mut self) -> Option<String> {
        let remote = self.active_remote();
        let table_id = self.panel_table(remote);
        let t = self.table_mut(table_id)?;
        let row = t.focused_row()?;
        Some(t.model().row_key(row).0)
    }

    /// The active panel's renamable row: the cursor row unless it is the
    /// `..` parent pseudo-row (a panel always renders `..` at row 0, and
    /// renaming it would target the parent directory). `targets()` filters
    /// `..` the same way for copy/Del.
    fn rename_target(&mut self) -> Option<String> {
        self.cursor_row_id().filter(|name| name != "..")
    }

    /// n/N/r/Ctrl+P: open the shared InputDialog with the prompt's prefill.
    fn open_prompt(&mut self, ctx: &mut Context) {
        let kind = match self.pending_prompt {
            Some(k) => k,
            None => return,
        };
        let prefill = match kind {
            PromptKind::NewDir | PromptKind::NewFile => String::new(),
            PromptKind::Rename => {
                // No row, or the `..` parent row: nothing to rename.
                let Some(name) = self.rename_target() else {
                    return;
                };
                name
            }
            PromptKind::Goto => self.active().path.clone(),
        };
        let (title, label) = match kind {
            PromptKind::NewDir => ("New directory", "Name"),
            PromptKind::NewFile => ("New file", "Name"),
            PromptKind::Rename => ("Rename", "New name"),
            PromptKind::Goto => ("Go to", "Path"),
        };
        let Some(requester) = self.state().id() else {
            return;
        };
        let dialog = crate::views::dialogs::InputDialog::new(
            prompt_dialog_rect(ctx),
            title,
            label,
            &prefill,
        );
        ctx.request_exec_view(Box::new(dialog), requester, Some(CMD_EXP_PROMPT_OK));
    }

    fn finish_prompt(&mut self) {
        let kind = match self.pending_prompt.take() {
            Some(k) => k,
            None => return,
        };
        if self.modal_answer.take() != Some(tv::Command::OK) {
            return;
        }
        let value = self.prompt_value.clone();
        if value.is_empty() {
            return;
        }
        let remote = self.active_remote();
        let dir = self.active().path.trim_end_matches('/').to_string();
        let target = if remote {
            self.target.clone()
        } else {
            ExplorerTarget::Host
        };
        match kind {
            PromptKind::NewDir => {
                let _ = self.task_tx.send(TaskReq::ExplorerCreate {
                    key: self.key.clone(),
                    target,
                    path: format!("{dir}/{value}"),
                    is_dir: true,
                });
            }
            PromptKind::NewFile => {
                let _ = self.task_tx.send(TaskReq::ExplorerCreate {
                    key: self.key.clone(),
                    target,
                    path: format!("{dir}/{value}"),
                    is_dir: false,
                });
            }
            PromptKind::Rename => {
                let Some(old) = self.rename_target() else {
                    return;
                };
                let _ = self.task_tx.send(TaskReq::ExplorerRename {
                    key: self.key.clone(),
                    target,
                    old: format!("{dir}/{old}"),
                    new: format!("{dir}/{value}"),
                });
            }
            PromptKind::Goto => {
                let panel = self.active_mut();
                panel.path = value.clone();
                panel.show_parent = value != "/";
                let _ = self.task_tx.send(TaskReq::ExplorerList {
                    key: self.key.clone(),
                    target,
                    path: value,
                });
            }
        }
    }
}

impl Drop for ExplorerWindow {
    fn drop(&mut self) {
        // The reopen-vs-focus flag (the Logs/Details pattern).
        *self.open.borrow_mut() = false;
        if let ExplorerTarget::Volume(name) = &self.target {
            let _ = self
                .task_tx
                .send(TaskReq::RemoveVolumeHelper { name: name.clone() });
        }
        if let ExplorerTarget::Image(image_id) = &self.target {
            let _ = self.task_tx.send(TaskReq::RemoveImageHelper {
                image: image_id.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests;
