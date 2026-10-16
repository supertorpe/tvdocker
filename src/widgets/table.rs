//! Multi-column table: header-click sorting, id-keyed multi-selection,
//! keyboard + wheel scrolling .
use std::collections::BTreeSet;
use tv::event::{Event, Key, MouseWheel};
use tv::{
    Command, CommandSet, Context, DrawCtx, FieldValue, HelpCtx, Options, Point, Rect, Role,
    Scroller, StateFlag, Style, View, ViewId, ViewState,
};

/// Column alignment inside its cell.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// One column definition: label, preferred width, alignment.
#[derive(Clone)]
pub struct Column {
    pub label: String,
    pub width: i32,
    pub align: Align,
}

impl Column {
    pub fn new(label: impl Into<String>, width: i32, align: Align) -> Self {
        Self {
            label: label.into(),
            width,
            align,
        }
    }
}

/// One display row: a stable id (selection/cursor identity,) plus
/// cell text; optional style for state coloring.
#[derive(Clone)]
pub struct TableRow {
    pub id: String,
    pub cells: Vec<String>,
    pub style: Option<Style>,
    /// Optional numeric key per column, index-aligned with `cells`
    /// (shorter vecs are fine — missing entries fall back to string
    /// order). `sort_rows` compares THESE when both rows carry one for
    /// the column: "10.2%" must sort after "9.9%".
    pub sort_keys: Vec<Option<f64>>,
}

impl TableRow {
    pub fn new(id: impl Into<String>, cells: Vec<String>) -> Self {
        Self {
            id: id.into(),
            cells,
            style: None,
            sort_keys: Vec::new(),
        }
    }
    pub fn with_sort_keys(mut self, keys: Vec<Option<f64>>) -> Self {
        self.sort_keys = keys;
        self
    }
}

/// Distribute `avail` over `cols`: honor wanted widths while they fit,
/// then shave one cell per pass from the currently-widest column until the
/// total (plus one gutter per column) fits. Non-positive avail → zeros.
pub fn layout_widths(cols: &[Column], avail: i32) -> Vec<i32> {
    let total_want: i32 = cols.iter().map(|c| c.width.max(0)).sum();
    let gutters = cols.len() as i32;
    let mut widths: Vec<i32> = cols.iter().map(|c| c.width.max(0)).collect();
    let budget = avail - gutters;
    if budget <= 0 || cols.is_empty() {
        return vec![0; cols.len()];
    }
    if total_want <= budget {
        return widths;
    }
    let mut over = total_want - budget;
    while over > 0 {
        let Some((i, &w)) = widths.iter().enumerate().max_by_key(|(_, w)| **w) else {
            break;
        };
        if w <= 1 {
            break;
        }
        widths[i] -= 1;
        over -= 1;
    }
    widths
}

/// Fit `cell` into exactly `width` display chars: pad with spaces, or cut
/// the tail with an ellipsis ("ellipsis truncation"):
/// `width 0 → ""`, `width 1 → "…"`, `width n → (n-1) chars + "…"`.
pub fn pad_cell(cell: &str, width: i32, align: Align) -> String {
    let width = width.max(0) as usize;
    if width == 0 {
        return String::new();
    }
    let len = cell.chars().count();
    if len <= width {
        let pad = " ".repeat(width - len);
        return match align {
            Align::Left => format!("{cell}{pad}"),
            Align::Right => format!("{pad}{cell}"),
        };
    }
    if width == 1 {
        return "…".to_string();
    }
    // Truncation cuts the tail regardless of alignment (the ellipsis marks
    // the cut); alignment only matters when padding.
    let head: String = cell.chars().take(width - 1).collect();
    format!("{head}…")
}

/// One rendered row of cells.
pub fn format_line(row: &TableRow, widths: &[i32], cols: &[Column]) -> String {
    let mut line = String::new();
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            line.push(' ');
        }
        let cell = row.cells.get(i).map_or("", String::as_str);
        let align = cols.get(i).map_or(Align::Left, |c| c.align);
        line.push_str(&pad_cell(cell, *w, align));
    }
    line
}

/// The header line; sorted column's label carries ▲/▼ .
pub fn header_line(cols: &[Column], widths: &[i32], sort: Option<(usize, bool)>) -> String {
    let mut line = String::new();
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            line.push(' ');
        }
        let Some(col) = cols.get(i) else { break };
        let label = match sort {
            Some((s, asc)) if s == i => {
                format!("{} {}", col.label, if asc { '▲' } else { '▼' })
            }
            _ => col.label.clone(),
        };
        line.push_str(&pad_cell(&label, *w, col.align));
    }
    line
}

/// Sort rows by column `col` (stable for ties): the NUMERIC key when
/// BOTH rows carry one for that column (`partial_cmp`, NaN ⇒ `Equal`),
/// else string order (— mixed pairs degrade to strings,
/// which no window produces: a column is keyed for all rows or none).
pub fn sort_rows(rows: &mut [TableRow], col: usize, asc: bool) {
    rows.sort_by(|a, b| {
        let ord = match (
            a.sort_keys.get(col).copied().flatten(),
            b.sort_keys.get(col).copied().flatten(),
        ) {
            (Some(ka), Some(kb)) => ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal),
            _ => a
                .cells
                .get(col)
                .map_or("", String::as_str)
                .cmp(b.cells.get(col).map_or("", String::as_str)),
        };
        if asc { ord } else { ord.reverse() }
    });
}

/// Clamp a desired cursor index into range (empty → 0).
pub fn target_cursor(current: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let base = current.min(len - 1) as i64;
    (base + i64::from(delta)).clamp(0, len as i64 - 1) as usize
}

/// Which column header sits under local x, if any.
fn column_at(widths: &[i32], x: i32) -> Option<usize> {
    let mut start = 1; // column 0 starts after the row-marker cell
    for (i, w) in widths.iter().enumerate() {
        let cell = *w + if i > 0 { 1 } else { 0 }; // gutter
        if x >= start && x < start + cell {
            return Some(i);
        }
        start += cell;
    }
    None
}

/// Table view . Bars are `ScrollBar` ids owned by the window;
/// the table is inserted (gets its own ViewId) and brokers bar events.
pub struct Table {
    st: ViewState,
    scroller: Scroller,
    columns: Vec<Column>,
    rows: Vec<TableRow>,
    cursor: usize,
    selected: BTreeSet<String>,
    sort: Option<(usize, bool)>,
    h_bar: Option<ViewId>,
    v_bar: Option<ViewId>,
    /// Columns hidden by the picker. Hidden ⇒ zero
    /// width, so every draw path (header, rows, sort marker) renders an
    /// empty cell without changing `format_line`'s index contract.
    hidden: BTreeSet<usize>,
    limit_dirty: bool,
    /// Whether this table is the one the keys reach — set by the window,
    /// which knows (the Explorer has two tables side by side). Defaults to
    /// `true`, so single-table windows are unaffected. Used instead of
    /// `state.focused`, which tvision-rs does not propagate to leaf tables
    /// in this app's tree (verified: a focused, key-receiving table still
    /// reports `focused == false`).
    active: bool,
}

impl Table {
    /// `v_bar` (and optional `h_bar`) are bar ids already inserted into the
    /// window; construct bars first (tvdir pattern).
    pub fn new(bounds: Rect, columns: Vec<Column>, v_bar: Option<ViewId>) -> Self {
        let rows_h = (bounds.b.y - bounds.a.y - 1).max(0);
        let mut st = ViewState::new(bounds);
        // Interactive: `Options::default()` is selectable:false (decorative).
        st.options = Options {
            selectable: true,
            first_click: true,
            ..Options::default()
        };
        Self {
            st,
            scroller: Scroller::new(
                Rect::new(0, 0, bounds.b.x - bounds.a.x, rows_h),
                None,
                v_bar,
            ),
            columns,
            rows: Vec::new(),
            cursor: 0,
            selected: BTreeSet::new(),
            sort: None,
            h_bar: None,
            v_bar,
            hidden: BTreeSet::new(),
            limit_dirty: false,
            active: true,
        }
    }

    /// Mark this table as the one receiving keys (`false` dims the cursor
    /// row — see the [`active`](Self::set_active) field docs).
    pub fn set_active(&mut self, on: bool) {
        self.active = on;
    }

    /// Whether this table is currently the keys' target.
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn selected_ids(&self) -> Vec<String> {
        self.selected.iter().cloned().collect()
    }
    pub fn sort_spec(&self) -> Option<(usize, bool)> {
        self.sort
    }
    pub fn scroll_offset(&self) -> i32 {
        self.scroller.delta.y
    }
    pub fn row_ids(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.id.clone()).collect()
    }

    /// Prime rows without a Context (examples, first paint); the scroll
    /// limit publishes on insert via `on_bounds_changed`.
    pub fn set_rows_initial(&mut self, rows: Vec<TableRow>) {
        self.rows = rows;
        self.cursor = 0;
        self.selected.clear();
        // No Context here, so the scroll limit cannot be published now —
        // `handle_event` publishes it on the next event (review C1 fix;
        // `Group::insert` does NOT call `on_bounds_changed`).
        self.limit_dirty = true;
    }

    /// Replace all rows: cursor follows the previously focused row id;
    /// selections for vanished ids are pruned .
    pub fn set_rows(&mut self, rows: Vec<TableRow>, ctx: &mut Context) {
        let keep = self.rows.get(self.cursor).map(|r| r.id.clone());
        self.rows = rows;
        self.cursor = match keep.and_then(|id| self.rows.iter().position(|r| r.id == id)) {
            Some(pos) => pos,
            None => target_cursor(self.cursor, 0, self.rows.len()),
        };
        self.selected
            .retain(|id| self.rows.iter().any(|r| r.id == *id));
        self.publish_limit(ctx);
        self.ensure_visible(ctx);
    }

    /// Sort by column `col` (`asc`), keeping the cursor on its row.
    pub fn sort_by(&mut self, col: usize, asc: bool, ctx: &mut Context) {
        let keep = self.rows.get(self.cursor).map(|r| r.id.clone());
        sort_rows(&mut self.rows, col, asc);
        if let Some(id) = keep
            && let Some(pos) = self.rows.iter().position(|r| r.id == id)
        {
            self.cursor = pos;
        } else {
            self.cursor = target_cursor(self.cursor, 0, self.rows.len());
        }
        self.sort = Some((col, asc));
        self.ensure_visible(ctx);
    }

    fn toggle_sort(&mut self, col: usize, ctx: &mut Context) {
        let asc = match self.sort {
            Some((c, a)) if c == col => !a,
            _ => true,
        };
        self.sort_by(col, asc, ctx);
    }

    /// Shift+←/→ : flip the direction on the CURRENT column
    /// without cycling; with no sort yet, establish the default sort
    /// (first column, ascending) so later presses toggle.
    fn flip_sort(&mut self, ctx: &mut Context) {
        let col = self.sort.map_or(0, |(c, _)| c);
        self.toggle_sort(col, ctx);
    }

    fn visible_rows(&self) -> usize {
        (self.st.size.y - 1).max(0) as usize
    }

    fn widths(&self) -> Vec<i32> {
        // Zero-width hidden columns: `layout_widths` distributes over
        // the rest, `pad_cell(.., 0)` renders "", and the gutter the
        // column still occupies is one stray space — cosmetic, price of
        // not rewriting every draw call.
        let cols: Vec<Column> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if self.hidden.contains(&i) {
                    Column::new(c.label.clone(), 0, c.align)
                } else {
                    c.clone()
                }
            })
            .collect();
        layout_widths(&cols, self.st.size.x - 1)
    }

    fn publish_limit(&mut self, ctx: &mut Context) {
        self.scroller.set_limit(0, self.rows.len() as i32, ctx);
        self.limit_dirty = false;
    }

    /// Scroll the minimal amount to show the cursor (arrows).
    fn ensure_visible(&mut self, ctx: &mut Context) {
        let vis = self.visible_rows() as i32;
        let off = self.scroller.delta.y;
        let cur = self.cursor as i32;
        if cur < off {
            self.apply_offset(cur, ctx);
        } else if cur >= off + vis {
            self.apply_offset(cur - vis + 1, ctx);
        }
    }

    /// Clamp into the scroll range, apply, and write the bars back.
    fn apply_offset(&mut self, want: i32, ctx: &mut Context) {
        let max = (self.scroller.limit().y - self.visible_rows() as i32).max(0);
        let want = want.clamp(0, max);
        self.scroller.apply_delta(Point::new(0, want));
        self.scroller.scroll_to(0, want, ctx);
    }

    fn move_to(&mut self, idx: usize, ctx: &mut Context) {
        self.cursor = if self.rows.is_empty() {
            0
        } else {
            idx.min(self.rows.len() - 1)
        };
        self.ensure_visible(ctx);
    }

    fn move_by(&mut self, delta: i32, ctx: &mut Context) {
        let idx = target_cursor(self.cursor, delta, self.rows.len());
        self.move_to(idx, ctx);
    }

    fn toggle_selected(&mut self) {
        let Some(row) = self.rows.get(self.cursor) else {
            return;
        };
        let id = row.id.clone();
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
    }

    /// Wheel scrolling: moves the view, not the cursor.
    fn scroll_by(&mut self, delta: i32, ctx: &mut Context) {
        self.apply_offset(self.scroller.delta.y + delta, ctx);
    }

    /// Clear every selection, keep the cursor (Esc rung 1,).
    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// Replace the selection with exactly `ids` (Ctrl+A: all filtered rows).
    pub fn select_ids(&mut self, ids: impl IntoIterator<Item = String>) {
        self.selected = ids.into_iter().collect();
    }

    /// Hide (or show) column `idx` — the column picker's apply step
    ///. The sort keeps its underlying index: a hidden
    /// sorted column still orders rows, its marker is simply invisible.
    pub fn set_column_hidden(&mut self, idx: usize, hidden: bool) {
        if idx >= self.columns.len() {
            return;
        }
        if hidden {
            self.hidden.insert(idx);
        } else {
            self.hidden.remove(&idx);
        }
    }

    /// Move the cursor to `idx` and keep it visible — right-click
    /// targeting needs the clicked row to BE the row the
    /// `CMD_REQ_*` handlers read.
    pub fn set_cursor_row(&mut self, idx: usize, ctx: &mut Context) {
        self.move_to(idx, ctx);
    }

    /// Column index for the next ←/→ cycle : skips hidden
    /// columns; `None` when every column is hidden.
    fn cycled_column(&self, delta: i32) -> Option<usize> {
        let n = self.columns.len();
        if n == 0 {
            return None;
        }
        let visible = |i: usize| !self.hidden.contains(&i);
        match self.sort {
            None => {
                // No sort yet: first (or last, backward) VISIBLE column.
                let mut probe = if delta >= 0 { 0i64 } else { n as i64 - 1 };
                for _ in 0..n {
                    let i = probe.rem_euclid(n as i64) as usize;
                    if visible(i) {
                        return Some(i);
                    }
                    probe += i64::from(delta);
                }
                None
            }
            Some((c, _)) => {
                let mut idx = c as i64;
                for _ in 0..n {
                    idx = (idx + i64::from(delta)).rem_euclid(n as i64);
                    let i = idx as usize;
                    if visible(i) {
                        return Some(i);
                    }
                }
                None
            }
        }
    }
}

impl View for Table {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }

    // The remaining View methods forward to the embedded `scroller`, exactly as
    // the `#[delegate(to = scroller)]` expansion did before tvision-rs 0.17's
    // guard required this impl to be written by hand: the Table keeps its own
    // ViewState (header+body rect) distinct from the scroller's body rect.
    // Throwaway: this file is deleted with the custom Table.
    fn set_state(&mut self, flag: StateFlag, enable: bool, ctx: &mut Context) {
        self.scroller.set_state(flag, enable, ctx);
    }
    fn valid(&mut self, cmd: Command, ctx: &mut Context) -> bool {
        self.scroller.valid(cmd, ctx)
    }
    fn set_modal_answer(&mut self, cmd: Command) {
        self.scroller.set_modal_answer(cmd)
    }
    fn value(&self) -> Option<FieldValue> {
        self.scroller.value()
    }
    fn set_value(&mut self, v: FieldValue) {
        self.scroller.set_value(v)
    }
    fn set_modal_data(&mut self, data: FieldValue) {
        self.scroller.set_modal_data(data)
    }
    fn set_value_ctx(&mut self, v: FieldValue, ctx: &mut Context) {
        self.scroller.set_value_ctx(v, ctx)
    }
    fn awaken(&mut self) {
        self.scroller.awaken()
    }
    fn size_limits(&self, owner_size: Point) -> (Point, Point) {
        self.scroller.size_limits(owner_size)
    }
    fn calc_bounds(&mut self, owner_size: Point, delta: Point) -> Rect {
        self.scroller.calc_bounds(owner_size, delta)
    }
    fn cursor_request(&self) -> Option<Point> {
        self.scroller.cursor_request()
    }
    fn caret_size(&self) -> u8 {
        self.scroller.caret_size()
    }
    fn find_mut(&mut self, id: ViewId) -> Option<&mut dyn View> {
        self.scroller.find_mut(id)
    }
    fn remove_descendant(&mut self, id: ViewId, ctx: &mut Context) -> bool {
        self.scroller.remove_descendant(id, ctx)
    }
    fn focus_descendant(&mut self, id: ViewId, ctx: &mut Context) -> bool {
        self.scroller.focus_descendant(id, ctx)
    }
    fn has_focusable_leaf(&self) -> bool {
        self.scroller.has_focusable_leaf()
    }
    fn focus_to_edge(&mut self, backward: bool, ctx: &mut Context) -> bool {
        self.scroller.focus_to_edge(backward, ctx)
    }
    fn reset_current(&mut self, ctx: &mut Context) {
        self.scroller.reset_current(ctx)
    }
    fn settle_currency(&mut self, ctx: &mut Context) {
        self.scroller.settle_currency(ctx)
    }
    fn set_visible_descendant(&mut self, id: ViewId, visible: bool, ctx: &mut Context) -> bool {
        self.scroller.set_visible_descendant(id, visible, ctx)
    }
    fn number(&self) -> Option<i16> {
        self.scroller.number()
    }
    fn grabs_focus_on_click(&self) -> bool {
        self.scroller.grabs_focus_on_click()
    }
    fn select_window_num(&mut self, num: i16, ctx: &mut Context) -> bool {
        self.scroller.select_window_num(num, ctx)
    }
    fn tile(&mut self, r: Rect) {
        self.scroller.tile(r)
    }
    fn cascade(&mut self, r: Rect) {
        self.scroller.cascade(r)
    }
    fn apply_scroll_sync(&mut self, h: Option<i32>, v: Option<i32>, ctx: &mut Context) {
        self.scroller.apply_scroll_sync(h, v, ctx)
    }
    fn update_menu_commands(&mut self, cs: &CommandSet) {
        self.scroller.update_menu_commands(cs)
    }
    fn set_menu_current(&mut self, current: Option<usize>) {
        self.scroller.set_menu_current(current)
    }
    fn get_help_ctx(&self) -> HelpCtx {
        self.scroller.get_help_ctx()
    }
    fn descendant_global_bounds(&self, id: ViewId, acc: Point) -> Option<Rect> {
        self.scroller.descendant_global_bounds(id, acc)
    }
    fn set_indicator_value(&mut self, location: Point, modified: bool) {
        self.scroller.set_indicator_value(location, modified)
    }
    fn apply_page_sync(&mut self, idx: usize, ctx: &mut Context) {
        self.scroller.apply_page_sync(idx, ctx)
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        ctx.fill(self.st.get_extent(), ' ', ctx.style(Role::ListSurface));
        let size = self.st.size;
        if size.x <= 1 || size.y <= 0 {
            return;
        }
        let widths = self.widths();
        let header_style = ctx.style(Role::ListFocused);
        ctx.put_str(
            1,
            0,
            &header_line(&self.columns, &widths, self.sort),
            header_style,
        );
        let vis = self.visible_rows();
        let off = self.scroller.delta.y.max(0) as usize;
        // The cursor row is a FOCUS signal: it is only drawn while this
        // table is the one receiving keys. The Explorer has two tables side
        // by side, and both used to claim the cursor, so there was no way to
        // tell which panel a keypress would reach.
        let active = self.active;
        for (i, row) in self.rows.iter().skip(off).take(vis).enumerate() {
            let idx = off + i;
            let cursor_here = idx == self.cursor && active;
            let selected = self.selected.contains(&row.id);
            let marker = if cursor_here {
                '>'
            } else if selected {
                '*'
            } else {
                ' '
            };
            let style = if cursor_here {
                ctx.style(Role::ListFocused)
            } else if selected {
                ctx.style(Role::ListSelected)
            } else {
                row.style.unwrap_or_else(|| ctx.style(Role::ListNormal))
            };
            ctx.put_char(0, 1 + i as i32, marker, style);
            let line = format_line(row, &widths, &self.columns);
            ctx.put_str(1, 1 + i as i32, &line, style);
        }
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        // Lazily publish a limit primed without a Context (review C1): must
        // run before ANY arm so the very first key/wheel already sees it.
        if self.limit_dirty {
            self.publish_limit(ctx);
        }
        // Bar → content: the embedded Scroller has no ViewId, so this
        // inserted composite brokers the sync (ListViewer pattern).
        if let Event::Broadcast { command, source } = &*ev {
            let own = source.is_some() && (*source == self.h_bar || *source == self.v_bar);
            if own && *command == Command::SCROLL_BAR_CLICKED {
                if let Some(id) = self.st.id() {
                    ctx.request_focus(id);
                }
            } else if own
                && *command == Command::SCROLL_BAR_CHANGED
                && let Some(id) = self.st.id()
            {
                ctx.request_scroll_sync(id, self.h_bar, self.v_bar);
            }
            return;
        }
        match ev {
            Event::KeyDown(ke) => {
                let key = ke.key;
                match key {
                    Key::Up => {
                        self.move_by(-1, ctx);
                        ev.clear();
                    }
                    Key::Down => {
                        self.move_by(1, ctx);
                        ev.clear();
                    }
                    Key::PageUp => {
                        self.move_by(-(self.visible_rows().max(1) as i32), ctx);
                        ev.clear();
                    }
                    Key::PageDown => {
                        self.move_by(self.visible_rows().max(1) as i32, ctx);
                        ev.clear();
                    }
                    Key::Home => {
                        self.move_to(0, ctx);
                        ev.clear();
                    }
                    Key::End => {
                        self.move_to(self.rows.len().saturating_sub(1), ctx);
                        ev.clear();
                    }
                    Key::Char(' ') => {
                        self.toggle_selected();
                        ev.clear();
                    }
                    Key::Right => {
                        if ke.modifiers.shift {
                            self.flip_sort(ctx);
                        } else if let Some(col) = self.cycled_column(1) {
                            self.sort_by(col, true, ctx);
                        }
                        ev.clear();
                    }
                    Key::Left => {
                        if ke.modifiers.shift {
                            self.flip_sort(ctx);
                        } else if let Some(col) = self.cycled_column(-1) {
                            self.sort_by(col, false, ctx);
                        }
                        ev.clear();
                    }
                    _ => {}
                }
            }
            Event::MouseWheel(m) => {
                let wheel = m.wheel;
                let delta = match wheel {
                    MouseWheel::Up => -3,
                    MouseWheel::Down => 3,
                    _ => 0,
                };
                if delta != 0 {
                    self.scroll_by(delta, ctx);
                    ev.clear();
                }
            }
            Event::MouseDown(m) => {
                let (x, y, left) = (m.position.x, m.position.y, m.buttons.left);
                if !left {
                    return;
                }
                if y == 0
                    && let Some(col) = column_at(&self.widths(), x)
                {
                    if !self.hidden.contains(&col) {
                        self.toggle_sort(col, ctx);
                    }
                    ev.clear();
                    return;
                }
                let idx = self.scroller.delta.y + y - 1;
                if (1..=self.visible_rows() as i32).contains(&y)
                    && idx >= 0
                    && (idx as usize) < self.rows.len()
                {
                    self.move_to(idx as usize, ctx);
                }
                ev.clear();
            }
            _ => {}
        }
    }

    fn change_bounds(&mut self, bounds: Rect) {
        self.st.set_bounds(bounds);
        let rows_h = (bounds.b.y - bounds.a.y - 1).max(0);
        self.scroller
            .change_bounds(Rect::new(0, 0, bounds.b.x - bounds.a.x, rows_h));
    }

    fn on_bounds_changed(&mut self, ctx: &mut Context) {
        self.publish_limit(ctx);
        self.ensure_visible(ctx);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{render_view, send_event, send_key, send_key_mods, text};
    use tv::event::{KeyModifiers, MouseEvent};

    fn cols() -> Vec<Column> {
        vec![
            Column::new("ID", 10, Align::Left),
            Column::new("NAME", 12, Align::Left),
            Column::new("STATE", 8, Align::Left),
        ]
    }
    fn row(id: &str, name: &str, state: &str) -> TableRow {
        TableRow::new(id, vec![id.into(), name.into(), state.into()])
    }
    fn table(h: i32) -> Table {
        Table::new(Rect::new(0, 0, 31, h), cols(), None)
    }
    fn rows3() -> Vec<TableRow> {
        vec![
            row("a", "one", "running"),
            row("b", "two", "exited"),
            row("c", "three", "running"),
        ]
    }
    fn with_rows(t: &mut Table, rows: Vec<TableRow>) {
        crate::testutil::with_ctx(|ctx| t.set_rows(rows, ctx));
    }
    fn click(x: i32, y: i32) -> Event {
        Event::MouseDown(MouseEvent {
            position: Point::new(x, y),
            buttons: tv::event::MouseButtons {
                left: true,
                ..Default::default()
            },
            ..Default::default()
        })
    }
    fn wheel(dir: MouseWheel) -> Event {
        Event::MouseWheel(MouseEvent {
            wheel: dir,
            ..Default::default()
        })
    }

    #[test]
    fn layout_widths_shrink_widest_until_it_fits() {
        // avail 30 − 2 gutters = 28; wants 60 → shave the widest per pass
        // until the total fits: converges to an equal split [14, 14].
        let w = layout_widths(
            &[
                Column::new("A", 40, Align::Left),
                Column::new("B", 20, Align::Left),
            ],
            30,
        );
        assert_eq!(w, vec![14, 14]);
        assert_eq!(w.iter().sum::<i32>() + 2, 30, "total + gutters == avail");
        // Total already fits → untouched.
        let w = layout_widths(
            &[
                Column::new("A", 5, Align::Left),
                Column::new("B", 5, Align::Left),
            ],
            30,
        );
        assert_eq!(w, vec![5, 5]);
    }

    #[test]
    fn layout_widths_nonpositive_avail_yields_zeros() {
        let w = layout_widths(&[Column::new("A", 40, Align::Left)], 0);
        assert_eq!(w, vec![0]);
    }

    #[test]
    fn format_line_pads_and_truncates_to_the_cell() {
        let cols = vec![
            Column::new("C1", 5, Align::Left),
            Column::new("C2", 8, Align::Left),
            Column::new("C3", 6, Align::Left),
        ];
        let widths = vec![4, 7, 5];
        let r = TableRow::new(
            "x",
            vec!["c1".into(), "very-long-name-here".into(), "running".into()],
        );
        assert_eq!(format_line(&r, &widths, &cols), "c1   very-l… runn…");
    }

    #[test]
    fn pad_cell_uses_ellipsis_truncation() {
        assert_eq!(pad_cell("abc", 0, Align::Left), "");
        assert_eq!(pad_cell("abc", 1, Align::Left), "…");
        assert_eq!(pad_cell("abc", 2, Align::Left), "a…");
        assert_eq!(pad_cell("abc", 3, Align::Left), "abc", "exact fit");
        assert_eq!(pad_cell("abc", 5, Align::Left), "abc  ");
        assert_eq!(pad_cell("abc", 5, Align::Right), "  abc");
        assert_eq!(
            pad_cell("abcdef", 4, Align::Right),
            "abc…",
            "tail cut keeps ellipsis at the cut"
        );
        // Multi-byte chars count as chars, not bytes.
        assert_eq!(pad_cell("日本語テキスト", 4, Align::Left), "日本語…");
        assert_eq!(pad_cell("ééé", 2, Align::Left), "é…");
    }

    #[test]
    fn header_line_marks_sort_direction() {
        let widths = vec![10, 12, 8];
        assert!(header_line(&cols(), &widths, Some((1, true))).contains("NAME ▲"));
        assert!(header_line(&cols(), &widths, Some((1, false))).contains("NAME ▼"));
        assert!(!header_line(&cols(), &widths, None).contains('▲'));
    }

    #[test]
    fn sort_rows_orders_strings_and_is_stable_for_ties() {
        let mut rows = vec![row("1", "b", "x"), row("2", "a", "y"), row("3", "b", "z")];
        sort_rows(&mut rows, 1, true);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["2", "1", "3"]
        );
        sort_rows(&mut rows, 1, false);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["1", "3", "2"]
        );
    }

    #[test]
    fn sort_rows_uses_numeric_keys_when_present() {
        let keyed = |id: &str, cell: &str, key: f64| {
            TableRow::new(id, vec![cell.to_string()]).with_sort_keys(vec![Some(key)])
        };
        let mut rows = vec![
            keyed("a", "9.9%", 9.9),
            keyed("b", "10.2%", 10.2),
            keyed("c", "1.5%", 1.5),
        ];
        sort_rows(&mut rows, 0, true);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["c", "a", "b"],
            "numeric: 1.5 < 9.9 < 10.2 — string order would say 10.2 < 1.5 < 9.9"
        );
        sort_rows(&mut rows, 0, false);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["b", "a", "c"],
            "descending mirrors"
        );
    }

    #[test]
    fn missing_sort_key_falls_back_to_string_order() {
        let unkeyed = |id: &str, cell: &str| TableRow::new(id, vec![cell.to_string()]);
        let mut rows = vec![unkeyed("x", "10"), unkeyed("y", "9")];
        sort_rows(&mut rows, 0, true);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["x", "y"],
            "no keys anywhere ⇒ pure string order"
        );
        // Mixed rows for one column: the pair falls back to string order
        // (never happens for a real window: a
        // column is keyed for ALL rows or for none).
        let mut mixed = vec![
            TableRow::new("n", vec!["10".to_string()]),
            TableRow::new("k", vec!["9".to_string()]).with_sort_keys(vec![Some(9.0)]),
        ];
        sort_rows(&mut mixed, 0, true);
        assert_eq!(
            mixed.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["n", "k"],
            "mixed ⇒ string compare of the cells (\"10\" < \"9\")"
        );
    }

    #[test]
    fn nan_sort_keys_treat_the_pair_as_equal_and_stay_stable() {
        let nan =
            |id: &str| TableRow::new(id, vec![id.to_string()]).with_sort_keys(vec![Some(f64::NAN)]);
        let mut rows = vec![nan("first"), nan("second")];
        sort_rows(&mut rows, 0, true);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["first", "second"],
            "NaN ⇒ Equal ⇒ stable order preserved"
        );
    }

    #[test]
    fn set_rows_keeps_cursor_on_the_same_row_id() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Down);
        send_key(&mut t, Key::Down); // cursor on "c"
        // Reverse the order; "c" is now first.
        let mut flipped = rows3();
        flipped.reverse();
        with_rows(&mut t, flipped);
        assert_eq!(t.cursor(), 0, "cursor followed row id 'c'");
    }

    #[test]
    fn set_rows_clamps_cursor_when_the_row_disappears() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::End);
        assert_eq!(t.cursor(), 2);
        with_rows(&mut t, vec![row("z", "solo", "running")]);
        assert_eq!(t.cursor(), 0, "clamped into the smaller list");
    }

    #[test]
    fn set_rows_prunes_stale_selections() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Char(' ')); // select "a"
        with_rows(&mut t, vec![row("b", "two", "exited")]);
        assert!(t.selected_ids().is_empty(), "stale id pruned");
    }

    #[test]
    fn cursor_keys_move_and_clamp() {
        let mut t = table(5); // visible rows = 4
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Down);
        assert_eq!(t.cursor(), 1);
        send_key(&mut t, Key::Down);
        send_key(&mut t, Key::Down);
        assert_eq!(t.cursor(), 2, "clamped at the last row");
        send_key(&mut t, Key::Home);
        assert_eq!(t.cursor(), 0);
        send_key(&mut t, Key::End);
        assert_eq!(t.cursor(), 2);
        send_key(&mut t, Key::PageDown);
        assert_eq!(t.cursor(), 2, "page down clamps");
        send_key(&mut t, Key::PageUp);
        assert_eq!(t.cursor(), 0, "page up clamps at 0");
    }

    #[test]
    fn space_toggles_selection_by_row_id() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Char(' '));
        assert_eq!(t.selected_ids(), vec!["a".to_string()]);
        send_key(&mut t, Key::Char(' '));
        assert!(t.selected_ids().is_empty());
    }

    #[test]
    fn unhandled_keys_bubble() {
        use tv::event::KeyEvent;
        let mut t = table(5);
        with_rows(&mut t, rows3());
        crate::testutil::with_ctx(|ctx| {
            let mut ev = Event::KeyDown(KeyEvent::new(Key::Char('x'), KeyModifiers::default()));
            t.handle_event(&mut ev, ctx);
            assert!(!ev.is_nothing(), "'x' must bubble to the owner");
        });
    }

    #[test]
    fn header_click_sorts_that_column_ascending_then_descending() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_event(&mut t, click(15, 0)); // NAME column (x ∈ [11, 22))
        assert_eq!(t.row_ids(), ["a", "c", "b"], "one < three < two");
        assert_eq!(t.sort_spec(), Some((1, true)));
        send_event(&mut t, click(15, 0));
        assert_eq!(t.row_ids(), ["b", "c", "a"]);
        assert_eq!(t.sort_spec(), Some((1, false)));
    }

    #[test]
    fn row_click_moves_cursor_and_out_of_range_is_ignored() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_event(&mut t, click(3, 2)); // y=2 → row index 1
        assert_eq!(t.cursor(), 1);
        send_event(&mut t, click(3, 40));
        assert_eq!(t.cursor(), 1, "outside the rows area: no move");
    }

    #[test]
    fn wheel_scrolls_when_content_is_long() {
        let mut t = table(5);
        with_rows(
            &mut t,
            (0..30).map(|i| row(&format!("id{i}"), "n", "s")).collect(),
        );
        send_event(&mut t, wheel(MouseWheel::Down));
        assert_eq!(t.scroll_offset(), 3);
        send_event(&mut t, wheel(MouseWheel::Down));
        send_event(&mut t, wheel(MouseWheel::Up));
        assert_eq!(t.scroll_offset(), 3, "up undid one step");
        send_event(&mut t, wheel(MouseWheel::Up));
        send_event(&mut t, wheel(MouseWheel::Up));
        assert_eq!(t.scroll_offset(), 0, "clamped at 0");
    }

    #[test]
    fn end_and_home_scroll_the_cursor_into_view() {
        let mut t = table(5); // visible = 4
        with_rows(
            &mut t,
            (0..20)
                .map(|i| row(&format!("id{i:02}"), "n", "s"))
                .collect(),
        );
        send_key(&mut t, Key::End);
        assert_eq!(t.scroll_offset(), 16, "cursor 19 visible in 4 rows");
        send_key(&mut t, Key::Home);
        assert_eq!(t.scroll_offset(), 0);
    }

    #[test]
    fn set_rows_initial_then_draw_shows_first_page() {
        let mut t = table(5);
        t.set_rows_initial(
            (0..50)
                .map(|i| row(&format!("id{i:02}"), "n", "s"))
                .collect(),
        );
        let out = text(&render_view(31, 5, &mut t));
        assert!(out.contains("id00"), "first row visible: {out:?}");
        assert!(!out.contains("id10"), "row 10 below the fold: {out:?}");
    }

    #[test]
    fn primed_table_publishes_limit_and_scrolls() {
        // Regression (review C1): `set_rows_initial` primes without a Context,
        // so the scroll limit must be published lazily on the first event —
        // otherwise End/wheel/PgDn are clamped to offset 0 on a fresh probe.
        let mut t = table(5); // visible = 4
        t.set_rows_initial(
            (0..20)
                .map(|i| row(&format!("id{i:02}"), "n", "s"))
                .collect(),
        );
        send_key(&mut t, Key::End);
        assert_eq!(t.cursor(), 19);
        assert_eq!(
            t.scroll_offset(),
            16,
            "limit published lazily on first event"
        );
        send_event(&mut t, wheel(MouseWheel::Up));
        assert_eq!(t.scroll_offset(), 13, "wheel works after priming");
    }

    #[test]
    fn as_any_mut_downcasts_to_table() {
        let mut t = table(5);
        let view: &mut dyn View = &mut t;
        assert!(
            view.as_any_mut()
                .is_some_and(|a| a.downcast_ref::<Table>().is_some())
        );
    }

    #[test]
    fn draw_renders_header_rows_marker_and_selection() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Char(' ')); // select "a" (cursor 0)
        send_key(&mut t, Key::Down); // cursor → row 1, "a" keeps '*'
        let screen = render_view(31, 5, &mut t);
        let out = text(&screen);
        assert!(out.contains("ID"), "header: {out:?}");
        assert!(out.contains('>'), "cursor marker: {out:?}");
        assert!(out.contains('*'), "selection marker: {out:?}");
        // Exact 31×5 render, inlined from the `insta` snapshot (the tool
        // and its `.snap` file are gone — the expectation belongs next to
        // the assertions it protects). NOTE: insta had trimmed this: it
        // stored neither row 5 nor the trailing column padding, so this is
        // stricter — and truthful — about what the widget draws.
        let expected = [
            " ID         NAME      STATE    ",
            "*a          one       running  ",
            ">b          two       exited   ",
            " c          three     running  ",
            "                               ", // row 5: blank, but still a row
        ]
        .join("\n");
        assert_eq!(out, format!("{expected}\n"));
    }

    #[test]
    fn clear_selection_empties_the_set_without_moving_the_cursor() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Char(' ')); // select "a"
        send_key(&mut t, Key::Down);
        t.clear_selection();
        assert!(t.selected_ids().is_empty());
        assert_eq!(t.cursor(), 1, "cursor untouched");
    }

    #[test]
    fn select_ids_replaces_the_selection_with_the_given_ids() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Char(' ')); // "a" selected
        t.select_ids(["b".to_string(), "c".to_string()]);
        assert_eq!(
            t.selected_ids(),
            vec!["b".to_string(), "c".to_string()],
            "Ctrl+A semantics: exactly the filtered set"
        );
    }

    #[test]
    fn left_and_right_cycle_the_sort_column() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Right);
        assert_eq!(
            t.sort_spec(),
            Some((0, true)),
            "first Right sorts column 0 ascending"
        );
        send_key(&mut t, Key::Right);
        assert_eq!(t.sort_spec(), Some((1, true)));
        send_key(&mut t, Key::Right);
        assert_eq!(t.sort_spec(), Some((2, true)));
        send_key(&mut t, Key::Right);
        assert_eq!(t.sort_spec(), Some((0, true)), "wraps around");
        send_key(&mut t, Key::Left);
        assert_eq!(
            t.sort_spec(),
            Some((2, false)),
            "Left walks backwards, sorting descending"
        );
    }

    fn shift() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        }
    }

    #[test]
    fn shift_arrow_flips_the_sort_direction_in_place_on_the_current_column() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Right); // ID ascending: a, b, c
        assert_eq!(t.sort_spec(), Some((0, true)));
        assert_eq!(t.row_ids(), ["a", "b", "c"]);
        send_key_mods(&mut t, Key::Right, shift());
        assert_eq!(
            t.sort_spec(),
            Some((0, false)),
            "Shift+→ flips the direction, keeping the column"
        );
        assert_eq!(t.row_ids(), ["c", "b", "a"], "order reversed");
        send_key_mods(&mut t, Key::Right, shift());
        assert_eq!(t.sort_spec(), Some((0, true)), "strict toggle back to asc");
        assert_eq!(t.row_ids(), ["a", "b", "c"]);
    }

    #[test]
    fn shift_left_flips_in_place_without_cycling_the_column() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Right);
        send_key(&mut t, Key::Right); // column 1 ascending
        assert_eq!(t.sort_spec(), Some((1, true)));
        send_key_mods(&mut t, Key::Left, shift());
        assert_eq!(
            t.sort_spec(),
            Some((1, false)),
            "Shift+← flips too — it must NOT walk to another column"
        );
    }

    #[test]
    fn shift_arrow_with_no_sort_starts_ascending_on_the_first_column() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key_mods(&mut t, Key::Right, shift());
        assert_eq!(
            t.sort_spec(),
            Some((0, true)),
            "nothing to flip yet → establish the default sort, later presses toggle"
        );
        // Distinguishes the shift path from plain Left (which would land on
        // the LAST column, descending — cycled_column(-1)).
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key_mods(&mut t, Key::Left, shift());
        assert_eq!(
            t.sort_spec(),
            Some((0, true)),
            "Shift+← with no sort must not walk backwards to the last column"
        );
    }

    #[test]
    fn a_direction_flip_keeps_the_cursor_on_its_row() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        send_key(&mut t, Key::Down);
        send_key(&mut t, Key::Down); // cursor on row "c"
        assert_eq!(t.row_ids()[t.cursor()], "c");
        send_key(&mut t, Key::Right); // ID ascending
        send_key_mods(&mut t, Key::Right, shift()); // → descending
        assert_eq!(t.row_ids(), ["c", "b", "a"]);
        assert_eq!(
            t.row_ids()[t.cursor()],
            "c",
            "the focused row must not jump when the direction flips"
        );
    }

    #[test]
    fn draw_degenerate_sizes_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (4, 1), (1, 4)] {
            let mut t = Table::new(Rect::new(0, 0, w, h), cols(), None);
            let screen = render_view(8, 8, &mut t);
            let _ = text(&screen);
        }
    }

    #[test]
    fn hiding_a_column_zeroes_its_width_in_header_and_rows() {
        let mut t = table(5); // cols ID(10), NAME(12), STATE(8) — fits 30
        with_rows(&mut t, rows3());
        t.set_column_hidden(1, true);
        assert_eq!(
            t.widths(),
            vec![10, 0, 8],
            "hidden ⇒ zero width; layout passes the rest through untouched"
        );
        let screen = text(&render_view(31, 5, &mut t));
        let header = screen.lines().next().unwrap_or_default();
        assert!(!header.contains("NAME"), "hidden header gone: {header:?}");
        assert!(
            header.contains("ID") && header.contains("STATE"),
            "others stay: {header:?}"
        );
        let row = screen.lines().nth(1).unwrap_or_default();
        assert!(!row.contains("one"), "hidden cell gone: {row:?}");
        assert!(row.contains("running"), "visible cells stay: {row:?}");

        t.set_column_hidden(1, false);
        assert_eq!(
            t.widths(),
            vec![10, 9, 8],
            "unhide: layout_widths shaves NAME 12→9"
        );
        assert!(text(&render_view(31, 5, &mut t)).contains("NAME"));
    }

    #[test]
    fn sort_cycling_skips_hidden_columns() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        t.set_column_hidden(0, true); // hide ID
        send_key(&mut t, Key::Right);
        assert_eq!(
            t.sort_spec(),
            Some((1, true)),
            "first visible column, never the hidden ID"
        );
        send_key(&mut t, Key::Left);
        assert_eq!(
            t.sort_spec(),
            Some((2, false)),
            "wraps backward past the hidden ID to STATE"
        );
    }

    #[test]
    fn hiding_every_column_disables_sort_cycling() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        t.set_column_hidden(0, true);
        t.set_column_hidden(1, true);
        t.set_column_hidden(2, true);
        send_key(&mut t, Key::Right);
        assert_eq!(t.sort_spec(), None, "nothing visible to sort by");
    }

    #[test]
    fn header_click_on_a_hidden_column_does_not_sort() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        t.set_column_hidden(1, true);
        // Column 0 is 10 wide (x 1..=10), so x=11 is NAME's slot — now a
        // zero-width shell whose gutter still maps to index 1.
        send_event(&mut t, click(11, 0));
        assert_eq!(t.sort_spec(), None, "hidden column cannot become sorted");
    }

    #[test]
    fn set_cursor_row_moves_the_cursor_for_right_click_targeting() {
        let mut t = table(5);
        with_rows(&mut t, rows3());
        crate::testutil::with_ctx(|ctx| t.set_cursor_row(2, ctx));
        assert_eq!(t.cursor(), 2, "the clicked row becomes the target row");
    }
    /// The cursor row is a FOCUS signal: with two tables side by side
    /// (the Explorer) both used to draw an active cursor, so there was no
    /// way to tell which panel the keys would reach.
    #[test]
    fn the_cursor_row_is_only_marked_while_the_table_is_active() {
        let mut t = table(4);
        with_rows(&mut t, rows3());

        // The active table (the default) marks its cursor row with '>'.
        assert!(t.is_active(), "a table starts active");
        let out = text(&render_view(31, 4, &mut t));
        assert!(out.contains('>'), "active table marks the cursor: {out:?}");

        // An inactive one stays quiet — it is not where the keys go.
        t.set_active(false);
        let out = text(&render_view(31, 4, &mut t));
        assert!(!out.contains('>'), "inactive table stays quiet: {out:?}");

        // …and recovers as soon as the window says so.
        t.set_active(true);
        let out = text(&render_view(31, 4, &mut t));
        assert!(out.contains('>'), "reactivated: {out:?}");
    }
}
