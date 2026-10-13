//! The Explorer window: host fs left,
//! container/volume right, both panels `Table`s over the vendored
//! explorer domain. The window owns a task-worker sender (the
//! StartShell precedent) and applies only path-matching listings.

use crate::core::{AppCore, ExplorerTarget, TaskReq};
use crate::domain::docker::explorer::ExplorerEntry;
use crate::widgets::{Align, Column, FilterBar, SharedLine, Table, TableRow};
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::mpsc;
use tv::event::{Event, Key};
use tv::menu::{Menu, popup_menu};
use tv::{Command, Context, DrawCtx, Rect, Role, View, ViewState, Window, delegate};

/// Help context while an Explorer window is focused .
pub const HELP_CTX: tv::HelpCtx = tv::HelpCtx::custom("tvdocker.explorer");

/// Window-menu number for Explorer (7/8/9 = Details/Logs/Shell).
pub const WINDOW_NUMBER: i16 = 10;

/// Two left clicks on the same row within this many milliseconds count as a
/// double-click (a click that opens the row).
///
/// The window counts clicks itself: tvision-rs 0.16's crossterm backend
/// builds every `MouseDown` with `..Default::default()`, so
/// `MouseEventFlags::double_click` is never set to `true` anywhere in the
/// dependency — a handler keyed on that flag can never fire.
const DOUBLE_CLICK_MS: u64 = 500;

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
    ExplorerRects {
        breadcrumb_host: Rect::new(1, 1, mid - 1, 2.min(bottom)),
        breadcrumb_remote: Rect::new(mid + 1, 1, right, 2.min(bottom)),
        host_filter: Rect::new(1, 2.min(bottom), mid - 1, 3.min(bottom)),
        remote_filter: Rect::new(mid + 1, 2.min(bottom), right, 3.min(bottom)),
        host_table: Rect::new(1, first_table, mid - 1, last_table),
        remote_table: Rect::new(mid + 1, first_table, right, last_table),
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
    remote_table: Rect,
    hint: Rect,
}

/// One panel's view half: table + its filter bar's id.
struct Panel {
    table: tv::ViewId,
    filter: tv::ViewId,
    path: String,
    entries: Vec<ExplorerEntry>,
    last_rev: u64,
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

/// The Explorer window: two `Table` panels over the host fs and the
/// container/volume, a shared hint row, and a task-worker sender for
/// list/copy/delete/create/rename requests (Tasks 9-10).
pub struct ExplorerWindow {
    win: Window,
    core: Rc<RefCell<AppCore>>,
    task_tx: mpsc::UnboundedSender<TaskReq>,
    key: String,
    target: ExplorerTarget,
    open: Rc<RefCell<bool>>,
    host: Panel,
    remote: Panel,
    /// Which side the printable keys / `/` / Del act on.
    active_remote: bool,
    /// The last left click — `(panel_is_remote, row, at_ms)` — for
    /// double-click detection (see [`DOUBLE_CLICK_MS`]).
    last_click: Option<(bool, usize, u64)>,
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
    /// Last laid-out size — the `PaneWindow::relayout_if_resized` probe.
    size: tv::Point,
    /// The prompt that is open (or about to be): create/rename/goto.
    pending_prompt: Option<PromptKind>,
    /// The pending Del's paths (the confirm round trip).
    pending_delete: Option<Vec<String>>,
    /// The answer of the last message box.
    modal_answer: Option<Command>,
    /// The shared prompt value.
    prompt_value: Rc<RefCell<String>>,
    /// Cached screen origin for right-click → `popup_menu` (the
    /// editor/`status_line` pattern).
    abs_origin: tv::Point,
}

impl ExplorerWindow {
    /// The home path for the host panel ($HOME, else "/").
    fn host_home() -> String {
        std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
    }

    fn active(&self) -> &Panel {
        if self.active_remote {
            &self.remote
        } else {
            &self.host
        }
    }

    fn active_mut(&mut self) -> &mut Panel {
        if self.active_remote {
            &mut self.remote
        } else {
            &mut self.host
        }
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
        if let Some(v) = self.win.child_mut(self.host.table)
            && let Some(t) = v.as_any_mut().and_then(|a| a.downcast_mut::<Table>())
        {
            t.set_rows(host_rows, ctx);
        }
        if let Some(v) = self.win.child_mut(self.remote.table)
            && let Some(t) = v.as_any_mut().and_then(|a| a.downcast_mut::<Table>())
        {
            t.set_rows(remote_rows, ctx);
        }
    }

    /// Re-derive the child rects whenever the window size changed (zoom /
    /// Size-Move drag / terminal resize — each ends in a redraw, which is
    /// where this runs, the `PaneWindow::relayout_if_resized` pattern).
    ///
    /// Without it the children keep their BUILD-time rects: `Group::
    /// change_bounds` only applies grow-mode deltas, and these children use
    /// the default (fixed) grow mode — so the panels froze at their original
    /// coordinates and overlapped the moved frame.
    fn relayout_if_resized(&mut self) {
        let size = self.win.state().size;
        if size == self.size {
            return;
        }
        self.size = size;
        let r = explorer_rects(size);
        for (id, rect) in [
            (self.breadcrumb_host_id, r.breadcrumb_host),
            (self.breadcrumb_remote_id, r.breadcrumb_remote),
            (self.host.filter, r.host_filter),
            (self.remote.filter, r.remote_filter),
            (self.host.table, r.host_table),
            (self.remote.table, r.remote_table),
            (self.hint_id, r.hint),
        ] {
            if let Some(v) = self.win.child_mut(id) {
                v.change_bounds(rect);
            }
        }
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
    // the frame's top border). Row 2: the two filters. Rows 3..h-2: the tables. Row h-2: the hint (row h-1 is the
    // frame's bottom border).
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
    let host_table = win.insert_child(Box::new(Table::new(r.host_table, columns(), None)));
    let remote_table = win.insert_child(Box::new(Table::new(r.remote_table, columns(), None)));
    let hint_id = win.insert_child(Box::new(SharedLine::new(
        r.hint,
        Rc::clone(&hint),
        Role::Info,
    )));

    let host_path = ExplorerWindow::host_home();
    let mut window = ExplorerWindow {
        win,
        core,
        task_tx,
        key: key.clone(),
        target,
        open,
        host: Panel {
            table: host_table,
            filter: host_filter,
            path: host_path.clone(),
            entries: Vec::new(),
            last_rev: 0,
            show_parent: host_path != "/",
        },
        remote: Panel {
            table: remote_table,
            filter: remote_filter,
            path: "/".to_string(),
            entries: Vec::new(),
            last_rev: 0,
            show_parent: false,
        },
        active_remote: false,
        last_click: None,
        breadcrumb_host,
        breadcrumb_remote,
        hint,
        breadcrumb_host_id,
        breadcrumb_remote_id,
        hint_id,
        size,
        pending_prompt: None,
        pending_delete: None,
        modal_answer: None,
        prompt_value: Rc::new(RefCell::new(String::new())),
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
    // The host side is active at open: only its table draws a cursor row.
    window.sync_panel_active();
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
        self.relayout_if_resized(); // children must track the window size
        self.abs_origin = ctx.origin(); // right-click → global coords
        self.win.draw(ctx);
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        self.sync_from_core();
        // --- Tab: exactly two stops (Ruling 3). The window intercepts
        // before the group's default traversal (which would also stop at
        // the selectable FilterBars). tv has no `BackTab` variant —
        // Shift+Tab is `Key::Tab` + shift — and the brief's tests use
        // plain Tab only.
        if let Event::KeyDown(ke) = ev
            && ke.key == Key::Tab
        {
            // A focused filter escapes back to its own table first.
            let active_filter = if self.active_remote {
                self.remote.filter
            } else {
                self.host.filter
            };
            let filter_focused = self
                .win
                .child_mut(active_filter)
                .is_some_and(|v| v.state().state.focused);
            let target = if filter_focused {
                self.active().table
            } else {
                self.active_remote = !self.active_remote;
                self.active().table
            };
            self.sync_panel_active();
            self.win.focus_descendant(target, ctx);
            self.redraw_texts(ctx);
            ev.clear();
            return;
        }
        // --- Esc: a focused filter escapes back to its table. When no
        // filter is focused, Esc falls through to the children (Task 9
        // adds the window-level Esc chain).
        if let Event::KeyDown(ke) = ev
            && ke.key == Key::Esc
        {
            let filter = if self.active_remote {
                self.remote.filter
            } else {
                self.host.filter
            };
            let filter_focused = self
                .win
                .child_mut(filter)
                .is_some_and(|v| v.state().state.focused);
            if filter_focused {
                let table = self.active().table;
                self.win.focus_descendant(table, ctx);
                ev.clear();
                return;
            }
        }
        // --- `/` focuses the active panel's filter .
        if let Event::KeyDown(ke) = ev
            && matches!(ke.key, Key::Char('/'))
        {
            let filter = self.active().filter;
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
                    if self.read_only() && !self.active_remote {
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
        // --- Mouse: a left click ACTIVATES the panel it lands on (and moves
        // that panel's cursor); a second click on the SAME row within
        // `DOUBLE_CLICK_MS` OPENS it — the file-manager convention, and the
        // same request Enter makes. Right-click opens the context menu (the
        // ResourceWindow precedent).
        //
        // The window counts the clicks itself because tvision-rs 0.16's
        // crossterm backend never sets `flags.double_click` (it builds every
        // MouseDown with `..Default::default()`), so that flag can never fire.
        if let Event::MouseDown(m) = ev {
            let m = *m;
            if m.buttons.left
                && let Some((remote, row)) = self.row_at(m.position)
            {
                let now = ctx.now_ms();
                let double = matches!(
                    self.last_click,
                    Some((p, r, at))
                        if p == remote && r == row && now.saturating_sub(at) <= DOUBLE_CLICK_MS
                );
                self.focus_panel_at(remote, row, ctx);
                if double {
                    self.last_click = None; // consumed: a triple click must not reopen
                    self.enter(ctx); // climbs/descends; a no-op on files
                } else {
                    self.last_click = Some((remote, row, now));
                }
                ev.clear();
                return;
            }
            if m.buttons.right
                && !m.buttons.left
                && let Some((remote, row)) = self.row_at(m.position)
            {
                self.focus_panel_at(remote, row, ctx);
                let items = self.context_menu_items();
                let mut builder = Menu::builder();
                for (label, command) in items {
                    builder = builder.command(label, command);
                }
                let global = m.position + self.abs_origin;
                popup_menu(global, builder.build(), ctx.owner_size(), ctx);
                ev.clear();
                return;
            }
        }
        self.redraw_texts(ctx);
        self.win.handle_event(ev, ctx);
    }

    fn set_modal_answer(&mut self, cmd: Command) {
        self.modal_answer = Some(cmd);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl ExplorerWindow {
    /// Enter: on the `..` row climb; on a dir row descend; files are
    /// no-ops (copy is the file action).
    fn enter(&mut self, ctx: &mut Context) {
        let table_id = self.active().table;
        let row = self
            .win
            .child_mut(table_id)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<Table>())
            .map(|t| t.cursor());
        let Some(cursor) = row else { return };
        let target = if self.active_remote {
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
            return; // files: no-op (Task 9 handles copy/delete)
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
        let target = if self.active_remote {
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

    /// Is the ACTIVE panel's filter bar focused? (gate for the printables)
    fn filter_focused(&mut self) -> bool {
        let filter = if self.active_remote {
            self.remote.filter
        } else {
            self.host.filter
        };
        self.win
            .child_mut(filter)
            .is_some_and(|v| v.state().state.focused)
    }

    /// Re-request the active panel's current listing (F5 / `R`).
    fn relist_active(&mut self) {
        let target = if self.active_remote {
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
        let table = self.active().table;
        if let Some(t) = self
            .win
            .child_mut(table)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<Table>())
        {
            let len = t.row_ids().len();
            let idx = if bottom { len.saturating_sub(1) } else { 0 };
            t.set_cursor_row(idx, ctx);
        }
    }

    /// The active panel's selection-or-cursor paths (selection wins).
    fn targets(&mut self) -> Vec<String> {
        let panel_dir = self.active().path.clone();
        let table_id = self.active().table;
        let Some(t) = self
            .win
            .child_mut(table_id)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<Table>())
        else {
            return Vec::new();
        };
        let selected = t.selected_ids();
        let ids = if selected.is_empty() {
            t.row_ids().get(t.cursor()).cloned().into_iter().collect()
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

    /// The context menu for the ACTIVE panel. An image window offers only
    /// copying out of the image plus refresh .
    fn context_menu_items(&self) -> Vec<(String, Command)> {
        if self.read_only() {
            let mut items = Vec::new();
            if self.active_remote {
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
        if self.read_only() && !self.active_remote {
            return;
        }
        let items = self.targets();
        if items.is_empty() {
            return;
        }
        let (target, to_container, dst) = if self.active_remote {
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

    /// The active table's cursor row id (absent when the table has no rows).
    fn cursor_row_id(&mut self) -> Option<String> {
        let table_id = self.active().table;
        self.win
            .child_mut(table_id)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<Table>())
            .and_then(|t| t.row_ids().get(t.cursor()).cloned())
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
        *self.prompt_value.borrow_mut() = prefill;
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
            Rc::clone(&self.prompt_value),
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
        let value = self.prompt_value.borrow().clone();
        if value.is_empty() {
            return;
        }
        let dir = self.active().path.trim_end_matches('/').to_string();
        let target = if self.active_remote {
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

    /// Window-local click → (is_remote, data-row index) for the context
    /// menu, mirroring the ResourceWindow `row_at_window_y` precedent.
    fn row_at(&mut self, pos: tv::Point) -> Option<(bool, usize)> {
        for remote in [false, true] {
            let table_id = if remote {
                self.remote.table
            } else {
                self.host.table
            };
            let bounds = match self.win.child_mut(table_id) {
                Some(v) => v.state().get_bounds(),
                None => continue,
            };
            if pos.x < bounds.a.x || pos.x >= bounds.b.x {
                continue;
            }
            let view = pos.y - bounds.a.y - 1; // −1 for the header
            if view < 0 {
                continue;
            }
            let Some(t) = self
                .win
                .child_mut(table_id)
                .and_then(|v| v.as_any_mut())
                .and_then(|a| a.downcast_mut::<Table>())
            else {
                continue;
            };
            let idx = t.scroll_offset() + view;
            let len = t.row_ids().len();
            if idx >= 0 && (idx as usize) < len {
                return Some((remote, idx as usize));
            }
        }
        None
    }

    /// Push `active_remote` into the two tables: only the key-receiving side
    /// draws its cursor row, so it is visible which panel is active.
    /// (`state.focused` would be the natural signal, but tvision-rs does not
    /// propagate it down to leaf tables in this app — a focused, key-receiving
    /// table still reports `focused == false`.)
    fn sync_panel_active(&mut self) {
        let (host_id, remote_id) = (self.host.table, self.remote.table);
        let remote_is_active = self.active_remote;
        for (id, active) in [(host_id, !remote_is_active), (remote_id, remote_is_active)] {
            if let Some(t) = self
                .win
                .child_mut(id)
                .and_then(|v| v.as_any_mut())
                .and_then(|a| a.downcast_mut::<Table>())
            {
                t.set_active(active);
            }
        }
    }

    /// Clicking a panel selects it, focuses its table and moves its cursor
    /// to the clicked row (the ResourceWindow precedent).
    fn focus_panel_at(&mut self, remote: bool, row: usize, ctx: &mut Context) {
        self.active_remote = remote;
        self.sync_panel_active();
        let table = self.active().table;
        self.win.focus_descendant(table, ctx);
        if let Some(t) = self
            .win
            .child_mut(table)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<Table>())
        {
            t.set_cursor_row(row, ctx);
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
