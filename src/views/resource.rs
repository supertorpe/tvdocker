//! Generic resource table window : Containers and the five resource
//! tables are ONE window driven by a static [`ResourceSpec`]
//! pane layout + table + fuzzy filter over shared
//! `AppCore` , with per-window identity in `help_ctx`
//! (contextual hotspots).

use crate::core::{AppCore, Connection};
use crate::domain::config::{ColumnLayoutCfg, TvdockerConfig};
use crate::widgets::pane::PaneWindow;
use crate::widgets::resource_table::{RowModel, TableRow, to_spec};
use crate::widgets::{Column, FilterBar};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key};
use tv::menu::{Menu, popup_menu};
use tv::widgets::table::TableModel;
use tv::widgets::{RowKey, SelectionMode, Separators, SortKey, Table};
use tv::{Command, Context, DrawCtx, HelpCtx, Rect, Role, View, ViewState, delegate};

/// A resource's context-menu builder: the window's core and the clicked
/// row's id in, the menu items out.
pub type ContextMenu = fn(&AppCore, &str) -> Vec<(String, Command)>;

/// A resource's raw sort-key source: `(model_col, keys)` pairs, the keys
/// index-aligned with the spec's [`rows`](ResourceSpec::rows).
pub type SortKeys = fn(&AppCore) -> Vec<(usize, Vec<SortKey>)>;

/// Everything that differs between resource windows.
/// Plain `fn` pointers + `&'static str` + `HelpCtx` — a `const` spec
/// lives in each window module.
#[derive(Clone, Copy)]
pub struct ResourceSpec {
    /// Window title (names).
    pub title: &'static str,
    /// Window-menu number (1 = Containers; the rest in menu order).
    pub number: i16,
    /// Help context the status line switches to while this window is
    /// focused (set once in `build`).
    pub help_ctx: HelpCtx,
    /// Which resource this window's delete acts on; `None` =
    /// the window has no delete binding at all (Events/Statistics).
    pub kind: Option<crate::core::PendingKind>,
    /// Table columns.
    pub columns: fn() -> Vec<Column>,
    /// ALL rows from `AppCore` — UNfiltered; the window owns fuzzy
    /// filtering over `TableRow.cells`.
    pub rows: fn(&AppCore) -> Vec<TableRow>,
    /// Live row count for the info line (`shown / total`) — a fn so the
    /// line tracks `AppCore` between refreshes.
    pub total: fn(&AppCore) -> usize,
    /// Info-line text when the list is empty (per window).
    pub empty: &'static str,
    /// Right-click menu items for a row of this resource (`None` =
    /// no menu). Labels may
    /// carry `~` hot markers (menus are hot-letter scoped,).
    pub context_menu: Option<ContextMenu>,
    /// Config key under which this table's column layout is persisted
    /// (`[column_layouts.<table_key>]`).
    pub table_key: &'static str,
    /// Raw per-column sort keys, index-aligned with [`rows`](Self::rows)
    /// (`(model_col, keys)`). Lets a formatted cell (e.g. `SIZE` "1.5GB")
    /// sort by its underlying value instead of its text.
    pub sort_keys: SortKeys,
}

/// Live counts for the info line.
pub struct InfoState {
    pub shown: usize,
    pub selected: usize,
    pub filter: String,
}

/// The pane's info row: connection state / empty state / counts
/// (filter row + §10 empty states). Connection text is shared;
/// the empty text and the denominator come from the spec.
pub struct InfoLine {
    st: ViewState,
    core: Rc<RefCell<AppCore>>,
    info: Rc<RefCell<InfoState>>,
    total: fn(&AppCore) -> usize,
    empty: &'static str,
    /// Whether the window owns multi-select (`spec.kind.is_some()`). Read-only
    /// windows (Events/Statistics) drop the `· N selected` segment — with no
    /// action there is nothing a selection could apply to.
    selectable: bool,
}

impl View for InfoLine {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        let core = self.core.borrow();
        let info = self.info.borrow();
        let (line, role) = match &core.connection {
            Connection::Down { .. } => {
                (" Docker unreachable — retrying…".to_string(), Role::Warning)
            }
            Connection::Checking => (" Docker: checking…".to_string(), Role::Info),
            _ if (self.total)(&core) == 0 => (self.empty.to_string(), Role::Info),
            _ => (
                format!(
                    " {} / {} shown{} · filter {}",
                    info.shown,
                    (self.total)(&core),
                    if self.selectable {
                        format!(" · {} selected", info.selected)
                    } else {
                        String::new()
                    },
                    if info.filter.is_empty() {
                        "-"
                    } else {
                        &info.filter
                    }
                ),
                Role::Info,
            ),
        };
        ctx.put_str(0, 0, &line, ctx.style(role));
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn matcher() -> SkimMatcherV2 {
    // Case-insensitive by requirement.
    SkimMatcherV2::default().ignore_case()
}

/// True when `needle` fuzzy-matches ANY cell (empty matches all). The
/// window's single filter path — containers' field-based helpers moved
/// here: the cells hold exactly those fields.
pub fn fuzzy_keep_row(row: &TableRow, needle: &str) -> bool {
    fuzzy_score_row(row, needle).is_some()
}

/// Best score across cells — the rank criterion (prefix > subsequence).
pub fn fuzzy_score_row(row: &TableRow, needle: &str) -> Option<i64> {
    if needle.is_empty() {
        return Some(0);
    }
    let m = matcher();
    row.cells
        .iter()
        .filter_map(|cell| m.fuzzy_match(cell, needle))
        .max()
}

/// tvdocker-style byte formatting for the Images/Volumes SIZE columns
/// (vendored thresholds verbatim): GB above 1e9, MB above
/// 1e6, KB above 1e3, else raw bytes.
pub fn format_size(bytes: i64) -> String {
    if bytes > 1_000_000_000 {
        format!("{:.1}GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes > 1_000_000 {
        format!("{:.1}MB", bytes as f64 / 1_000_000.0)
    } else if bytes > 1_000 {
        format!("{:.1}KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes}B")
    }
}

/// Build the row [`RowModel`] for `needle`: the spec's rows filtered by the
/// fuzzy needle, plus the spec's raw per-column sort keys filtered in lockstep
/// (so a formatted cell such as `SIZE` "1.5GB" still sorts by its bytes).
fn model_for(spec: &ResourceSpec, core: &AppCore, needle: &str) -> RowModel {
    let all = (spec.rows)(core);
    let keys = (spec.sort_keys)(core);
    let keep: Vec<usize> = (0..all.len())
        .filter(|&i| fuzzy_keep_row(&all[i], needle))
        .collect();
    let rows: Vec<TableRow> = keep.iter().map(|&i| all[i].clone()).collect();
    let mut model = RowModel::new(rows);
    for (col, col_keys) in keys {
        let filtered: Vec<SortKey> = keep
            .iter()
            .map(|&i| {
                col_keys.get(i).cloned().unwrap_or_else(|| {
                    SortKey::parse(all[i].cells.get(col).map_or("", String::as_str))
                })
            })
            .collect();
        model = model.with_sort_keys(col, filtered);
    }
    model
}

/// The window: `PaneWindow` wrapper that refilters on REFRESH / sink
/// changes, owns the Esc priority chain  and Ctrl+A.
pub struct ResourceWindow {
    pane: PaneWindow,
    spec: ResourceSpec,
    core: Rc<RefCell<AppCore>>,
    sink: Rc<RefCell<String>>,
    info: Rc<RefCell<InfoState>>,
    last_revision: u64,
    open: Rc<RefCell<bool>>,
    /// Cached screen origin for right-click/Shift+F10 → `popup_menu` (the
    /// editor/status_line `abs_origin` pattern, `editor.rs:1917`).
    abs_origin: tv::Point,
}

/// Build the window (bounds are the window's outer rect; `filter`
/// prefills the filter line — CLI prefill, passed only for
/// Containers).
pub fn build(
    spec: ResourceSpec,
    core: Rc<RefCell<AppCore>>,
    bounds: Rect,
    filter: Option<&str>,
    open: Rc<RefCell<bool>>,
) -> ResourceWindow {
    let sink = Rc::new(RefCell::new(filter.unwrap_or_default().to_string()));
    // The prefilled CLI filter applies from the FIRST paint :
    // initial rows honor the same needle as every later refilter.
    let needle = filter.unwrap_or_default();
    let initial_model = model_for(&spec, &core.borrow(), needle);
    let initial_shown = initial_model.row_count();
    let info = Rc::new(RefCell::new(InfoState {
        shown: initial_shown,
        selected: 0,
        filter: sink.borrow().clone(),
    }));
    let last_revision = core.borrow().revision;
    // The persisted per-table column layout, re-read once per build.
    let layout = TvdockerConfig::load()
        .0
        .column_layout(spec.table_key)
        .cloned();
    // Selection is the action-target picker: only windows that own an action
    // (`kind: Some`) offer multi-select. Read-only windows (Events/Statistics)
    // get `Single` — no check column, no "selected" counter.
    let selectable = spec.kind.is_some();
    let mut pane = PaneWindow::new(
        bounds,
        spec.title,
        spec.number,
        Box::new(InfoLine {
            st: ViewState::new(Rect::new(0, 0, 1, 1)),
            core: Rc::clone(&core),
            info: Rc::clone(&info),
            total: spec.total,
            empty: spec.empty,
            selectable,
        }),
        Box::new(FilterBar::new(
            Rect::new(0, 0, 1, 1),
            Rc::clone(&sink),
            "type to filter",
        )),
        |rect, bar| {
            let columns = (spec.columns)().iter().map(to_spec).collect();
            let mut table = Table::new(rect, columns, bar)
                .with_separators(Separators::All)
                .with_selection_mode(if selectable {
                    SelectionMode::Multi
                } else {
                    SelectionMode::Single
                })
                .with_model(Box::new(initial_model));
            // The upstream `Table` is a `Group`: mark it selectable so the
            // window's hierarchical Tab pass treats it as one focusable stop
            // (its body is the focusable leaf inside). Without this the group
            // is skipped and Tab can never leave/enter the table.
            table.state_mut().options.selectable = true;
            if let Some(cfg) = &layout {
                table.set_column_layout(&cfg.to_states());
            }
            Box::new(table)
        },
    );
    // Set ONCE on the window's state — the pump resolves it through the
    // root → … → Window-group chain (`Group::get_help_ctx` falls back to
    // its own state when the focused leaf yields NO_CONTEXT).
    pane.state_mut().help_ctx = spec.help_ctx;
    ResourceWindow {
        pane,
        spec,
        core,
        sink,
        info,
        last_revision,
        open,
        abs_origin: tv::Point::new(0, 0),
    }
}

impl ResourceWindow {
    /// Test/introspection hook: current table row ids, in display order.
    pub fn table_ids(&mut self) -> Vec<String> {
        self.table_mut().map_or_else(Vec::new, |t| {
            let order = t.row_order();
            let model = t.model();
            order.iter().map(|&row| model.row_key(row).0).collect()
        })
    }

    /// Whether this window owns multi-select: true when it has an action
    /// (`kind: Some`) for the marked rows to target. Read-only windows
    /// (Events/Statistics) return false — their table is `Single`.
    fn selectable(&self) -> bool {
        self.spec.kind.is_some()
    }

    fn table_mut(&mut self) -> Option<&mut Table> {
        let body = self.pane.ids().body;
        self.pane
            .window_mut()
            .child_mut(body)
            .and_then(|v| v.as_any_mut().and_then(|a| a.downcast_mut::<Table>()))
    }

    fn filter_mut(&mut self) -> Option<&mut FilterBar> {
        let id = self.pane.ids().filter;
        self.pane
            .window_mut()
            .child_mut(id)
            .and_then(|v| v.as_any_mut().and_then(|a| a.downcast_mut::<FilterBar>()))
    }

    /// Select exactly the filtered set (Ctrl+A). Both delivery
    /// paths converge here — the `KeyDown` arm below and the
    /// pre-route's `Event::Command(CMD_SELECT_ALL)`. The model already
    /// holds only the filtered rows, so selecting every model row is the
    /// filtered set.
    fn select_all(&mut self) {
        if let Some(table) = self.table_mut() {
            let keys: Vec<RowKey> = {
                let model = table.model();
                (0..model.row_count())
                    .map(|row| model.row_key(row))
                    .collect()
            };
            table.set_selected_keys(keys);
        }
        self.sync_info();
    }

    /// Re-map core → rows (applying the active fuzzy filter; the upstream
    /// `set_model` re-applies the active sort and re-focuses by `RowKey`),
    /// then refresh the info counts.
    /// Called on REFRESH or sink change (the "last filter" guard).
    fn refresh_rows(&mut self, ctx: &mut Context) {
        let needle = self.sink.borrow().clone();
        let model = {
            let core = self.core.borrow();
            model_for(&self.spec, &core, &needle)
        };
        let shown = model.row_count();
        if let Some(table) = self.table_mut() {
            table.set_model(Box::new(model), ctx);
        }
        let selected = self.table_mut().map_or(0, |t| t.selected_keys().len());
        let mut info = self.info.borrow_mut();
        info.shown = shown;
        info.filter = needle;
        info.selected = selected;
    }

    /// Push the table's selection count into the info line.
    fn sync_info(&mut self) {
        let selected = self.table_mut().map_or(0, |t| t.selected_keys().len());
        self.info.borrow_mut().selected = selected;
    }

    /// The cursor row's id (window openers — F2/F4/Enter).
    fn open_target(&mut self) -> Vec<String> {
        let Some(t) = self.table_mut() else {
            return Vec::new();
        };
        t.focused_key().map(|k| k.0).into_iter().collect()
    }

    /// Selection if any, else the cursor row (Del, §7 menu).
    fn action_targets(&mut self) -> Vec<String> {
        let Some(t) = self.table_mut() else {
            return Vec::new();
        };
        let selected: Vec<String> = t.selected_keys().into_iter().map(|k| k.0).collect();
        if !selected.is_empty() {
            return selected;
        }
        t.focused_key().map(|k| k.0).into_iter().collect()
    }

    /// The display row (an index into `Table::row_order`) under window-local
    /// `y`, or `None` when the point is on the header row or past the last
    /// row. `t` must be focused: the upstream `Table` exposes no scroll
    /// offset, so `top_item` is recovered from the focused cell's cursor —
    /// `cursor_request` is table-local with the header at row 0, so its `y`
    /// is the body origin (1) plus the focused row's offset into the body.
    fn display_row_at(t: &Table, y: i32) -> Option<usize> {
        let view_y = y - t.state().get_bounds().a.y - 1; // −1: the header row
        if view_y < 0 {
            return None;
        }
        let cursor = t.cursor_request()?;
        let order = t.row_order();
        let focused = t.focused_row()?;
        let display_focused = order.iter().position(|&row| row == focused)?;
        let top_item = display_focused as i32 - (cursor.y - 1);
        usize::try_from(top_item + view_y)
            .ok()
            .filter(|&display| display < order.len())
    }

    /// The focused display row (an index into `Table::row_order`), or `None`.
    fn focused_display_row(&mut self) -> Option<usize> {
        let t = self.table_mut()?;
        let order = t.row_order();
        let focused = t.focused_row()?;
        order.iter().position(|&row| row == focused)
    }

    /// Focus display row `row` on the table (a no-op when it already holds
    /// focus, but always re-publishes the row to the vertical bar).
    fn focus_display_row(&mut self, row: usize, ctx: &mut Context) {
        let Some(t) = self.table_mut() else {
            return;
        };
        let key = {
            let order = t.row_order();
            let model = t.model();
            order.get(row).map(|&r| model.row_key(r))
        };
        if let Some(key) = key {
            t.set_focused_key(&key, ctx);
        }
    }

    /// Deliver a press into the pane through a throwaway `Context`: the group
    /// focuses the clicked row exactly as upstream would, while the mouse-hold
    /// capture the press arms is written to the scratch queue and discarded (a
    /// right-click is not a drag; leaving that track live would swallow the
    /// command the context menu posts on release).
    fn forward_for_focus(&mut self, ev: &mut Event) {
        let mut out = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred = Vec::new();
        let mut scratch = Context::new(&mut out, &mut timers, 0, &mut deferred);
        self.pane.handle_event(ev, &mut scratch);
    }

    /// Cycle the sort column on ←/→ (backward walks the other way and lands
    /// DESCENDING, as the old table did), or flip the direction on
    /// Shift+←/→. The upstream `Table` only exposes `TABLE_SORT_NEXT` (forward,
    /// ascending) and `TABLE_SORT_FLIP`, so the window keeps tvdocker's
    /// documented cycle and drives [`Table::sort_by`] itself.
    fn cycle_sort(&mut self, forward: bool, flip: bool, ctx: &mut Context) {
        if let Some(t) = self.table_mut() {
            crate::widgets::cycle_table_sort(t, forward, flip, ctx);
        }
    }

    /// Open the row context menu at window-local `anchor`, built from
    /// the row the table just focused.
    fn open_context_menu_at(&mut self, anchor: tv::Point, ctx: &mut Context) {
        let Some(menu) = self.spec.context_menu else {
            return;
        };
        let id = self.open_target().into_iter().next().unwrap_or_default();
        if id.is_empty() {
            return;
        }
        let items = menu(&self.core.borrow(), &id);
        if items.is_empty() {
            return;
        }
        let mut builder = Menu::builder();
        for (label, command) in items {
            builder = builder.command(label, command);
        }
        let global = anchor + self.abs_origin;
        popup_menu(global, builder.build(), ctx.owner_size(), ctx);
    }

    /// Where the keyboard route (Shift+F10) opens the menu: the CURSOR
    /// row's first cell — the point a right-click on that row would use
    /// (the keyboard has only the cursor; a right-click targets the
    /// CLICKED row). The caret is the only cursor the upstream Table
    /// exposes and it is hidden while the filter bar holds focus, so that
    /// case falls back to the table's top-left; the menu's TARGET is the
    /// cursor row either way (`open_target` reads the focused row).
    fn context_anchor(&mut self) -> Option<tv::Point> {
        let t = self.table_mut()?;
        let bounds = t.state().get_bounds();
        let cursor = t.cursor_request().unwrap_or(tv::Point::new(0, 1));
        Some(tv::Point::new(bounds.a.x + cursor.x, bounds.a.y + cursor.y))
    }
}

#[delegate(to = pane)]
impl View for ResourceWindow {
    fn state(&self) -> &ViewState {
        self.pane.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.pane.state_mut()
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.abs_origin = ctx.origin(); // right-click → global coords
        self.pane.draw(ctx);
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        // --- The upstream Table arranged its columns (resize / move /
        // show / hide): persist the new layout for this window's key.
        if let Event::Broadcast { command, source } = ev
            && *command == Command::TABLE_COLUMNS_CHANGED
            && *source == Some(self.pane.ids().body)
        {
            let states = self
                .table_mut()
                .map(|t| t.column_layout())
                .unwrap_or_default();
            let _ = TvdockerConfig::set_column_layout_in_file(
                &TvdockerConfig::path(),
                self.spec.table_key,
                &ColumnLayoutCfg::from_states(&states),
            );
            ev.clear();
            return;
        }
        // --- Esc priority chain , before anything else sees it:
        if let Event::KeyDown(ke) = ev
            && ke.key == Key::Esc
        {
            if let Some(table) = self.table_mut()
                && !table.selected_keys().is_empty()
            {
                table.clear_selection(); // rung 1: exit multi-select
                self.sync_info();
                ev.clear();
                return;
            }
            if !self.sink.borrow().is_empty() {
                // Rung 2: clear the FILTER BAR itself — its `text` drives
                // the drawing; clearing only the sink would leave stale
                // characters on screen (caught by the Esc-chain test).
                if let Some(fb) = self.filter_mut() {
                    fb.clear(); // syncs the sink as a side effect
                } else {
                    self.sink.borrow_mut().clear();
                }
                self.refresh_rows(ctx);
                ev.clear();
                return;
            }
            // Rung 3: close the active window (the framework has no
            // Esc→close path — verified; this is ours).
            if let Some(id) = self.state().id() {
                ctx.request_close(id);
                ev.clear();
            }
            return;
        }
        // --- Ctrl+A: select exactly the filtered set . The
        // status pre-route turns the chord into `Event::Command`;
        // the KeyDown arm covers unit tests and
        // any pre-route miss — both end in `select_all()`.
        if let Event::KeyDown(ke) = ev
            && ke.modifiers.ctrl
            && matches!(ke.key, Key::Char('a') | Key::Char('A'))
            && self.selectable()
        {
            self.select_all();
            ev.clear();
            return;
        }
        // --- The "Image" jump's data hop: a focus request queued in the
        // core, delivered by broadcast (the desktop holds only a
        // `ViewId` — no way to reach a window's table directly). Only the
        // Images window acts; every other window leaves the request
        // queued for its recipient.
        if let Event::Broadcast { command, .. } = ev
            && *command == crate::app::CMD_FOCUS_REQUEST
            && self.spec.kind == Some(crate::core::PendingKind::Images)
        {
            let requested = self.core.borrow_mut().take_focus_request();
            if let Some(row_id) = requested {
                let key = RowKey(row_id);
                let present = self.table_mut().is_some_and(|t| {
                    let model = t.model();
                    (0..model.row_count()).any(|r| model.row_key(r) == key)
                });
                if !present {
                    // The row is hidden behind an active filter — the
                    // explicit jump outweighs it (Esc/type-to-filter
                    // restores a filter anyway).
                    if !self.sink.borrow().is_empty() {
                        if let Some(fb) = self.filter_mut() {
                            fb.clear();
                        } else {
                            self.sink.borrow_mut().clear();
                        }
                        self.refresh_rows(ctx);
                    }
                }
                if let Some(t) = self.table_mut() {
                    t.set_focused_key(&key, ctx);
                }
            }
            ev.clear();
            return;
        }
        // --- The status pre-route's form of the same chord:
        // KeyDown → Event::Command(CMD_SELECT_ALL) when the contextual def's
        // key binding is active. Identical behavior, shared `select_all()`.
        if let Event::Command(cmd) = ev
            && *cmd == crate::app::CMD_SELECT_ALL
            && self.selectable()
        {
            self.select_all();
            ev.clear();
            return;
        }
        // --- Ctrl+O (re-pointed to the framework's `TABLE_COLUMNS`): the
        // upstream Table opens its header popup. Hand the command to the
        // table directly so it works even while the filter bar holds focus.
        if let Event::Command(cmd) = ev
            && *cmd == Command::TABLE_COLUMNS
        {
            if let Some(t) = self.table_mut() {
                t.handle_event(ev, ctx);
            }
            ev.clear();
            return;
        }

        // --- Sort keys (←/→ cycle the column, Shift flips the direction):
        // the window owns them because the upstream `Table` command layer only
        // has forward-ascending and flip. Only when the table body holds the
        // focus (the filter bar keeps its own ←/→ caret movement).
        if let Event::KeyDown(ke) = ev
            && matches!(ke.key, Key::Left | Key::Right)
            && self
                .table_mut()
                .is_some_and(|t| t.cursor_request().is_some())
        {
            self.cycle_sort(ke.key == Key::Right, ke.modifiers.shift, ctx);
            ev.clear();
            return;
        }

        // --- Shift+F10: the mouse-free context menu — the same popup a
        // right-click opens (§7), anchored at the CURSOR row's cell. The
        // status pre-route never transforms it (bound nowhere), so it
        // arrives here as a plain KeyDown, filter bar focus included.
        if let Event::KeyDown(ke) = ev
            && ke.key == Key::F(10)
            && ke.modifiers.shift
        {
            if let Some(anchor) = self.context_anchor() {
                self.open_context_menu_at(anchor, ctx);
            }
            ev.clear();
            return;
        }

        // --- mint the targets the
        // desktop will consume, then re-post ITS command.
        if let Event::Command(cmd) = ev {
            let second = if *cmd == crate::app::CMD_REQ_DETAILS {
                Some(crate::app::CMD_OPEN_DETAILS)
            } else if *cmd == crate::app::CMD_REQ_LOGS {
                Some(crate::app::CMD_OPEN_LOGS)
            } else if *cmd == crate::app::CMD_REQ_EXPLORER {
                Some(crate::app::CMD_EXPLORER)
            } else if *cmd == crate::app::CMD_REQ_VOLUME_EXPLORER {
                Some(crate::app::CMD_VOLUME_EXPLORER)
            } else if *cmd == crate::app::CMD_REQ_IMAGE_EXPLORER {
                Some(crate::app::CMD_IMAGE_EXPLORER)
            } else if *cmd == crate::app::CMD_REQ_IMAGE {
                Some(crate::app::CMD_SHOW_IMAGE)
            } else if *cmd == crate::app::CMD_REQ_IMAGE_LAYERS {
                Some(crate::app::CMD_LAYERS)
            } else if *cmd == crate::app::CMD_REQ_SHELL {
                Some(crate::app::CMD_SHELL_CONFIG)
            } else if *cmd == crate::app::CMD_REQ_DIAGNOSTICS {
                Some(crate::app::CMD_DIAGNOSTICS)
            } else if *cmd == crate::app::CMD_REQ_DELETE {
                Some(crate::app::CMD_DELETE)
            } else if *cmd == crate::app::CMD_REQ_START {
                Some(crate::app::CMD_START)
            } else if *cmd == crate::app::CMD_REQ_STOP {
                Some(crate::app::CMD_STOP)
            } else {
                None
            };
            // Run… takes the CURSOR row (one form per
            // invocation); the batch sweeps carry NO targets but still
            // ride the one confirm flow (`CMD_DELETE` + `PendingKind`).
            if *cmd == crate::app::CMD_REQ_RUN {
                let targets = self.open_target();
                if !targets.is_empty() {
                    self.core
                        .borrow_mut()
                        .set_pending(targets, crate::core::PendingKind::Images);
                    ctx.post(crate::app::CMD_RUN);
                }
                ev.clear();
                return;
            }
            if *cmd == crate::app::CMD_REQ_DANGLING || *cmd == crate::app::CMD_REQ_PRUNE {
                let kind = if *cmd == crate::app::CMD_REQ_DANGLING {
                    crate::core::PendingKind::DanglingImages
                } else {
                    crate::core::PendingKind::PruneImages
                };
                self.core.borrow_mut().set_pending(Vec::new(), kind);
                ctx.post(crate::app::CMD_DELETE);
                ev.clear();
                return;
            }
            if let Some(out) = second {
                // Windows openers take the CURSOR row (F2/F4 for
                // "the selected container" — singular); destructive/state
                // actions take selection-or-cursor (§5 Del "delete
                // selected", §7 context menu applies to the set).
                let targets = if matches!(
                    *cmd,
                    crate::app::CMD_REQ_DETAILS
                        | crate::app::CMD_REQ_LOGS
                        | crate::app::CMD_REQ_SHELL
                        | crate::app::CMD_REQ_EXPLORER
                        | crate::app::CMD_REQ_IMAGE
                        | crate::app::CMD_REQ_VOLUME_EXPLORER
                        | crate::app::CMD_REQ_DIAGNOSTICS
                        | crate::app::CMD_REQ_IMAGE_EXPLORER
                        | crate::app::CMD_REQ_IMAGE_LAYERS
                ) {
                    self.open_target()
                } else {
                    self.action_targets()
                };
                if !targets.is_empty() {
                    // The kind rides along with the targets: only the
                    // delete confirm reads it, but it always truthfully
                    // describes what the queue holds.
                    self.core
                        .borrow_mut()
                        .set_pending(targets, self.spec.kind.unwrap_or_default());
                    ctx.post(out);
                }
                ev.clear();
                return;
            }
        }
        // --- Right-click: a press on the header is handed to the table (its
        // column popup); a press on a body row focuses the clicked row and
        // opens our context menu for it. The row is resolved from the click
        // geometry rather than by forwarding the press, because forwarding
        // would arm the body's mouse-hold capture — which would then swallow
        // the very command the menu posts on release.
        if let Event::MouseDown(m) = ev
            && m.buttons.right
            && !m.buttons.left
        {
            let m = *m;
            let header_y = self.table_mut().map(|t| t.state().get_bounds().a.y);
            if header_y.is_some_and(|y| m.position.y > y) {
                // Body press. A focused table resolves the row from its
                // cursor; an unfocused one has no cursor to read `top_item`
                // from, so it goes through the (scratch) routing that focuses
                // the clicked row.
                let focused = self
                    .table_mut()
                    .is_some_and(|t| t.cursor_request().is_some());
                let row = if focused {
                    self.table_mut()
                        .and_then(|t| Self::display_row_at(t, m.position.y))
                } else {
                    self.forward_for_focus(ev);
                    self.focused_display_row()
                };
                if let Some(row) = row {
                    self.focus_display_row(row, ctx);
                    self.open_context_menu_at(m.position, ctx);
                }
            } else {
                // Header (or above): the table opens its own column popup.
                self.pane.handle_event(ev, ctx);
            }
            ev.clear();
            return;
        } else {
            self.pane.handle_event(ev, ctx);
        }

        // --- Re-map when the bridge bumped the revision, or the sink moved.
        let revision = self.core.borrow().revision;
        let sink_changed = {
            let want = self.sink.borrow().clone();
            want != self.info.borrow().filter
        };
        if revision != self.last_revision || sink_changed {
            self.last_revision = revision;
            self.refresh_rows(ctx);
        }
        self.sync_info();
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Drop for ResourceWindow {
    /// The desktop uses this flag to decide reopen-vs-focus.
    fn drop(&mut self) {
        *self.open.borrow_mut() = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AppMsg, Connection};
    use crate::testutil::{render_view, send_event, send_key, send_key_mods, text};
    use tv::event::{Key, KeyModifiers};
    use tv::widgets::SortDir;

    /// A spec built from scratch proves the window is generic: any
    /// `fn(&AppCore) -> Vec<TableRow>` source drives it.
    fn demo_spec() -> ResourceSpec {
        ResourceSpec {
            title: "Demo",
            number: 9,
            help_ctx: HelpCtx::custom("tvdocker.demo"),
            columns: || vec![Column::new("NAME", 12, crate::widgets::Align::Left)],
            rows: |core| {
                core.volumes
                    .iter()
                    .map(|v| TableRow::new(v.name.clone(), vec![v.name.clone()]))
                    .collect()
            },
            total: |core| core.volumes.len(),
            empty: " No demos.",
            kind: None,
            context_menu: None,
            table_key: "demo",
            sort_keys: |_| Vec::new(),
        }
    }

    /// A spec with an action (`kind: Some`) — the shape Containers and the
    /// four pickers have.
    fn actionable_demo_spec() -> ResourceSpec {
        ResourceSpec {
            kind: Some(crate::core::PendingKind::Volumes),
            ..demo_spec()
        }
    }

    /// Selection is the action-target picker: a window with no action
    /// (`kind: None` — Events/Statistics) must NOT get the upstream
    /// multi-select check column, nor the "selected" info-line segment.
    #[test]
    fn read_only_windows_get_no_check_column_and_no_selected_count() {
        let core = core_with(&["alpha", "beta"]);
        let mut w = window(demo_spec(), core); // kind: None
        let out = text(&render_view(76, 23, &mut w));
        assert_eq!(
            w.table_mut().expect("table child").selection_mode(),
            SelectionMode::Single,
            "no action ⇒ single selection"
        );
        assert!(!out.contains("[ ]"), "no check column drawn: {out:?}");
        assert!(!out.contains("selected"), "no selected counter: {out:?}");
        assert!(out.contains(" 2 / 2 shown"), "counts still shown: {out:?}");
    }

    /// The mirror: an actionable window keeps multi-select and its counter.
    #[test]
    fn actionable_windows_keep_multi_select_and_the_selected_count() {
        let core = core_with(&["alpha", "beta"]);
        let mut w = window(actionable_demo_spec(), core);
        let out = text(&render_view(76, 23, &mut w));
        assert_eq!(
            w.table_mut().expect("table child").selection_mode(),
            SelectionMode::Multi,
            "an action ⇒ multi selection"
        );
        assert!(out.contains("[ ]"), "check column drawn: {out:?}");
        assert!(out.contains("selected"), "selected counter: {out:?}");
    }

    /// 0.18: the window's table rules its column separators — the header
    /// and every row split their cells with the theme's `│`.
    #[test]
    fn tables_render_column_separators() {
        let core = core_with(&["alpha", "beta"]);
        let spec = ResourceSpec {
            columns: || {
                vec![
                    Column::new("NAME", 12, crate::widgets::Align::Left),
                    Column::new("SIZE", 8, crate::widgets::Align::Right),
                ]
            },
            rows: |core| {
                core.volumes
                    .iter()
                    .map(|v| TableRow::new(v.name.clone(), vec![v.name.clone(), "1MB".into()]))
                    .collect()
            },
            ..demo_spec()
        };
        let mut w = window(spec, core);
        assert_eq!(
            w.table_mut().expect("table child").separators(),
            tv::widgets::Separators::All,
            "the resource table draws its column separators"
        );
        let out = text(&render_view(76, 23, &mut w));
        assert!(
            out.contains("NAME        │"),
            "the header rules between the columns: {out:?}"
        );
        assert!(
            out.contains("alpha       │"),
            "row cells are ruled apart: {out:?}"
        );
    }

    /// Ctrl+A on a read-only window must not silently populate invisible
    /// marks (which would then consume the first Esc).
    #[test]
    fn ctrl_a_is_inert_on_a_read_only_window() {
        let core = core_with(&["alpha", "beta"]);
        let mut w = window(demo_spec(), core);
        send_key_mods(
            &mut w,
            Key::Char('a'),
            KeyModifiers {
                ctrl: true,
                ..KeyModifiers::default()
            },
        );
        assert!(
            w.table_mut()
                .expect("table child")
                .selected_keys()
                .is_empty(),
            "no marks on a window with no action"
        );
    }

    fn core_with(names: &[&str]) -> Rc<RefCell<AppCore>> {
        let core = Rc::new(RefCell::new(AppCore::new()));
        let rows = names
            .iter()
            .map(|n| crate::domain::docker::volumes::VolumeEntry {
                name: (*n).into(),
                driver: "local".into(),
                mountpoint: String::new(),
                size: 0,
            })
            .collect();
        core.borrow_mut().apply(AppMsg::Volumes(rows));
        core.borrow_mut().apply(AppMsg::Connection(Connection::Up));
        core
    }

    fn window(spec: ResourceSpec, core: Rc<RefCell<AppCore>>) -> ResourceWindow {
        build(
            spec,
            core,
            Rect::new(2, 1, 78, 23),
            None,
            Rc::new(RefCell::new(true)),
        )
    }

    #[test]
    fn window_maps_rows_from_any_spec_and_refilters_on_refresh() {
        let core = core_with(&["alpha", "beta"]);
        let mut w = window(demo_spec(), core.clone());
        assert_eq!(w.table_ids(), vec!["alpha", "beta"], "initial rows");

        // A poll lands: core changes, bridge broadcasts REFRESH.
        core.borrow_mut().apply(AppMsg::Volumes(vec![
            crate::domain::docker::volumes::VolumeEntry {
                name: "gamma".into(),
                driver: "local".into(),
                mountpoint: String::new(),
                size: 0,
            },
        ]));
        send_event(
            &mut w,
            Event::Broadcast {
                command: crate::bridge::REFRESH,
                source: None,
            },
        );
        assert_eq!(w.table_ids(), vec!["gamma"], "re-mapped on revision");
    }

    #[test]
    fn info_line_uses_the_specs_empty_text() {
        let core = core_with(&[]);
        let mut w = window(demo_spec(), core);
        let out = text(&render_view(76, 23, &mut w));
        assert!(
            out.contains(" No demos."),
            "empty text comes from the spec: {out:?}"
        );
    }

    #[test]
    fn fuzzy_keep_accepts_prefix_subsequence_and_case_and_drops_junk() {
        // Same criteria set as the containers tests (the cells hold
        // exactly the fields the matcher reads).
        let r = TableRow::new(
            "1",
            vec![
                "web-server".into(),
                "img".into(),
                "running".into(),
                "Up".into(),
                "80/tcp".into(),
            ],
        );
        assert!(fuzzy_keep_row(&r, "web"), "prefix matches");
        assert!(fuzzy_keep_row(&r, "wsr"), "subsequence matches (w…s…r)");
        assert!(fuzzy_keep_row(&r, "WEB-SERVER"), "case-insensitive");
        assert!(fuzzy_keep_row(&r, ""), "empty matches all");
        assert!(!fuzzy_keep_row(&r, "zzz"), "junk is dropped");
    }

    #[test]
    fn fuzzy_score_ranks_prefix_above_subsequence() {
        let r = TableRow::new("1", vec!["web-server".into(), "running".into()]);
        let prefix = fuzzy_score_row(&r, "web").expect("prefix scores");
        let subseq = fuzzy_score_row(&r, "wsr").expect("subsequence scores");
        assert!(
            prefix > subseq,
            "rank criterion: prefix {prefix} > subsequence {subseq}"
        );
    }

    #[test]
    fn format_size_follows_tvdocker_thresholds() {
        assert_eq!(format_size(1_500_000_000), "1.5GB");
        assert_eq!(format_size(1_500_000), "1.5MB");
        assert_eq!(format_size(1_500), "1.5KB");
        assert_eq!(format_size(999), "999B");
        assert_eq!(
            format_size(1_000_000_000),
            "1000.0MB",
            "verbatim vendored boundary: the test uses `>`"
        );
    }

    #[test]
    fn refilter_preserves_sort_and_cursor() {
        // Criteria unchanged — it now drives the generic window through
        // the real Containers spec.
        let core = Rc::new(RefCell::new(AppCore::new()));
        let row = |id: &str, name: &str, state: &str| {
            crate::domain::docker::containers::ContainerSummary {
                id: id.into(),
                name: name.into(),
                image: "img".into(),
                image_id: "sha256:img".into(),
                state: state.into(),
                status: "Up".into(),
                ports: "80/tcp".into(),
                project: String::new(),
                health: String::new(),
            }
        };
        core.borrow_mut().apply(AppMsg::Containers(vec![
            row("id1", "web", "running"),
            row("id2", "db", "exited"),
            row("id3", "cache", "running"),
        ]));
        let mut w = crate::views::containers::build(
            core,
            Rect::new(2, 1, 78, 23),
            None,
            Rc::new(RefCell::new(true)),
        );
        {
            let t = w.table_mut().expect("table child");
            // Sort col 0 (NAME) DESCENDING directly (←/→ cycle forward-
            // ascending by design — header-click or sort_by picks
            // direction): NAME desc = web > db > cache = id1, id2, id3.
            // Then put the cursor on the last row (id3); `set_focused_key`
            // is the deterministic, focus-independent way (the upstream
            // body only routes End when it holds focus).
            crate::testutil::with_ctx(|ctx| {
                t.sort_by(0, SortDir::Desc, ctx);
                t.set_focused_key(&RowKey("id3".to_string()), ctx);
            });
        }
        assert_eq!(
            w.table_ids(),
            vec!["id1", "id2", "id3"],
            "NAME descending: web > db > cache"
        );
        w.sink.borrow_mut().push_str("running"); // the user typed a filter
        send_key(&mut w, Key::Char('k')); // any event → after_event → refilter
        assert_eq!(
            w.table_ids(),
            vec!["id1", "id3"],
            "cache+web match 'running'"
        );
        let t = w.table_mut().expect("table child");
        assert_eq!(
            t.sort_spec(),
            Some((0, SortDir::Desc)),
            "sort survived the refilter"
        );
        assert_eq!(
            t.focused_key().expect("focused row").0,
            "id3",
            "cursor stayed on its row identity across the refilter"
        );
    }

    /// A window over three containers, filtered to the two `running` ones —
    /// the shared setup for both Ctrl+A paths (Review Focus #2).
    fn filtered_window() -> ResourceWindow {
        let core = Rc::new(RefCell::new(AppCore::new()));
        let row = |id: &str, name: &str, state: &str| {
            crate::domain::docker::containers::ContainerSummary {
                id: id.into(),
                name: name.into(),
                image: "img".into(),
                image_id: "sha256:img".into(),
                state: state.into(),
                status: "Up".into(),
                ports: "80/tcp".into(),
                project: String::new(),
                health: String::new(),
            }
        };
        core.borrow_mut().apply(AppMsg::Containers(vec![
            row("id1", "web", "running"),
            row("id2", "db", "exited"),
            row("id3", "cache", "running"),
        ]));
        let mut w = crate::views::containers::build(
            core,
            Rect::new(2, 1, 78, 23),
            None,
            Rc::new(RefCell::new(true)),
        );
        w.sink.borrow_mut().push_str("running");
        send_event(
            &mut w,
            Event::Broadcast {
                command: crate::bridge::REFRESH,
                source: None,
            },
        );
        assert_eq!(w.table_ids(), vec!["id1", "id3"], "needle applied");
        w
    }

    #[test]
    fn ctrl_a_keydown_selects_all_filtered_rows() {
        // Unit path: the window's own KeyDown arm (no pre-route involved —
        // testutil drives the view directly).
        let mut w = filtered_window();
        send_key_mods(
            &mut w,
            Key::Char('a'),
            KeyModifiers {
                ctrl: true,
                ..KeyModifiers::default()
            },
        );
        assert_eq!(
            w.table_mut()
                .expect("table child")
                .selected_keys()
                .into_iter()
                .map(|k| k.0)
                .collect::<Vec<_>>(),
            vec!["id1", "id3"],
            "exactly the FILTERED rows"
        );
    }

    #[test]
    fn ctrl_a_command_selects_all_filtered_rows() {
        // Pre-route path: what the status line emits after transforming the
        // chord — identical outcome via `select_all()`.
        let mut w = filtered_window();
        send_event(&mut w, Event::Command(crate::app::CMD_SELECT_ALL));
        assert_eq!(
            w.table_mut()
                .expect("table child")
                .selected_keys()
                .into_iter()
                .map(|k| k.0)
                .collect::<Vec<_>>(),
            vec!["id1", "id3"],
            "exactly the FILTERED rows"
        );
    }

    /// The Diagnose menu request mints the CURSOR row and re-posts the
    /// desktop's opener command.
    #[test]
    fn two_hop_diagnose_mints_the_cursor_row_and_posts_the_desktop_command() {
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut().apply(AppMsg::Containers(vec![
            crate::domain::docker::containers::ContainerSummary {
                id: "id-a".into(),
                name: "web".into(),
                image: "img".into(),
                image_id: "sha256:img".into(),
                state: "running".into(),
                status: "Up".into(),
                ports: "80/tcp".into(),
                project: String::new(),
                health: String::new(),
            },
        ]));
        core.borrow_mut().apply(AppMsg::Connection(Connection::Up));
        let mut w = crate::views::containers::build(
            Rc::clone(&core),
            Rect::new(2, 1, 78, 23),
            None,
            Rc::new(RefCell::new(true)),
        );
        let mut out = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        {
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            let mut ev = Event::Command(crate::app::CMD_REQ_DIAGNOSTICS);
            w.handle_event(&mut ev, &mut ctx);
        }
        assert_eq!(
            core.borrow().pending_targets,
            vec!["id-a".to_string()],
            "one cursor-row target"
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Command(c) if *c == crate::app::CMD_DIAGNOSTICS)),
            "the desktop's command was re-posted: {out:?}"
        );
    }

    /// The Images Explorer request mints the cursor row and re-posts the
    /// desktop's opener command.
    #[test]
    fn two_hop_image_explorer_mints_the_cursor_row_and_posts_the_opener() {
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut().apply(AppMsg::Images(vec![
            crate::domain::docker::images::ImageEntry {
                id: "sha256:abc".into(),
                repository: "alpine".into(),
                tag: "latest".into(),
                size: 1_000,
            },
        ]));
        let mut w = crate::views::images::build(
            Rc::clone(&core),
            Rect::new(2, 1, 78, 23),
            Rc::new(RefCell::new(true)),
        );
        let mut out = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        {
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            let mut ev = Event::Command(crate::app::CMD_REQ_IMAGE_EXPLORER);
            w.handle_event(&mut ev, &mut ctx);
        }
        assert_eq!(
            core.borrow().pending_targets,
            vec!["sha256:abc".to_string()],
            "one cursor-row target"
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Command(c) if *c == crate::app::CMD_IMAGE_EXPLORER)),
            "the desktop's command was re-posted"
        );
    }

    /// The Layers request mints the cursor row and re-posts the desktop's
    /// opener command (the image-explorer two-hop, repeated).
    #[test]
    fn two_hop_image_layers_mints_the_cursor_row_and_posts_the_opener() {
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut().apply(AppMsg::Images(vec![
            crate::domain::docker::images::ImageEntry {
                id: "sha256:abc".into(),
                repository: "alpine".into(),
                tag: "latest".into(),
                size: 1_000,
            },
        ]));
        let mut w = crate::views::images::build(
            Rc::clone(&core),
            Rect::new(2, 1, 78, 23),
            Rc::new(RefCell::new(true)),
        );
        let mut out = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        {
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            let mut ev = Event::Command(crate::app::CMD_REQ_IMAGE_LAYERS);
            w.handle_event(&mut ev, &mut ctx);
        }
        assert_eq!(
            core.borrow().pending_targets,
            vec!["sha256:abc".to_string()],
            "one cursor-row target"
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Command(c) if *c == crate::app::CMD_LAYERS)),
            "the desktop's command was re-posted"
        );
    }

    /// The "Image" request mints the CURSOR container row and re-posts
    /// the desktop's opener (the two-hop, like Explorer).
    #[test]
    fn two_hop_image_mints_the_cursor_container_and_posts_the_desktop_opener() {
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut().apply(AppMsg::Containers(vec![
            crate::domain::docker::containers::ContainerSummary {
                id: "id-a".into(),
                name: "web".into(),
                image: "img".into(),
                image_id: "sha256:img".into(),
                state: "running".into(),
                status: "Up".into(),
                ports: String::new(),
                project: String::new(),
                health: String::new(),
            },
        ]));
        let mut w = crate::views::containers::build(
            Rc::clone(&core),
            Rect::new(2, 1, 78, 23),
            None,
            Rc::new(RefCell::new(true)),
        );
        let mut out = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        {
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            let mut ev = Event::Command(crate::app::CMD_REQ_IMAGE);
            w.handle_event(&mut ev, &mut ctx);
        }
        assert_eq!(
            core.borrow().pending_targets,
            vec!["id-a".to_string()],
            "one cursor-row container id"
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Command(c) if *c == crate::app::CMD_SHOW_IMAGE)),
            "the desktop's opener was re-posted: {out:?}"
        );
    }

    /// The focus-request broadcast moves the Images window's cursor to
    /// the queued row — and is inert on other windows and when nothing
    /// is queued.
    #[test]
    fn focus_broadcast_moves_the_images_cursor_and_is_inert_elsewhere() {
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut().apply(AppMsg::Images(vec![
            crate::domain::docker::images::ImageEntry {
                id: "sha256:aaa".into(),
                repository: "nginx".into(),
                tag: "1.27".into(),
                size: 1_000,
            },
            crate::domain::docker::images::ImageEntry {
                id: "sha256:bbb".into(),
                repository: "redis".into(),
                tag: "latest".into(),
                size: 1_000,
            },
        ]));
        let broadcast = |w: &mut ResourceWindow, core: &Rc<RefCell<AppCore>>| {
            core.borrow_mut()
                .set_focus_request("sha256:bbb".to_string());
            let mut ev = Event::Broadcast {
                command: crate::app::CMD_FOCUS_REQUEST,
                source: None,
            };
            let mut out = std::collections::VecDeque::new();
            let mut timers = tv::TimerQueue::new();
            let mut deferred: Vec<tv::Deferred> = Vec::new();
            let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
            w.handle_event(&mut ev, &mut ctx);
        };

        // The Images window: cursor jumps to the requested row.
        let mut images = crate::views::images::build(
            Rc::clone(&core),
            Rect::new(2, 1, 78, 23),
            Rc::new(RefCell::new(true)),
        );
        broadcast(&mut images, &core);
        assert_eq!(
            images
                .table_mut()
                .and_then(|t| t.focused_key())
                .map(|k| k.0),
            Some("sha256:bbb".to_string()),
            "cursor on the requested image row"
        );

        // A Containers window: the broadcast is inert (its kind differs) —
        // and the request stays queued for the real recipient.
        let containers_core = Rc::new(RefCell::new(AppCore::new()));
        containers_core
            .borrow_mut()
            .apply(AppMsg::Containers(vec![]));
        let mut containers = crate::views::containers::build(
            Rc::clone(&containers_core),
            Rect::new(2, 1, 78, 23),
            None,
            Rc::new(RefCell::new(true)),
        );
        broadcast(&mut containers, &containers_core);
        assert_eq!(
            containers_core.borrow().focus_request,
            Some("sha256:bbb".to_string()),
            "a non-Images window must not consume the request"
        );
    }
}
