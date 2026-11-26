//! Tail-following log viewport with hard memory caps .
use std::collections::VecDeque;
use tv::TrackMask;
use tv::event::{Event, Key, MouseWheel};
use tv::{
    Command, Context, DrawCtx, Options, Point, Rect, Role, Scroller, StateFlag, View, ViewId,
    delegate,
};

use super::selection::{TextPos, TextSelection};

/// Max stored bytes per line (/ deviation #3); longer lines are
/// cut on a char boundary and suffixed with `…`.
pub const MAX_LINE_BYTES: usize = 2048;

/// Cut an over-long line to ≤ [`MAX_LINE_BYTES`] bytes, keeping a char
/// boundary and appending `…`.
pub fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_LINE_BYTES {
        return line.to_string();
    }
    // Reserve the 3 bytes of `…` itself.
    let mut cut = MAX_LINE_BYTES - 3;
    while cut > 0 && !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &line[..cut])
}

/// Remove ANSI escape sequences (CSI `ESC [ … letter`, OSC `ESC ] … BEL|ST`,
/// and a bare `ESC x`) — "ANSI escapes stripped at ingest"
///. Character-agnostic: multi-byte text passes through
/// untouched; a dangling trailing `ESC` is dropped rather than stored as
/// garbage.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                // CSI: parameters/intermediates until a final byte
                // in the 0x40..=0x7E range.
                while let Some(&n) = chars.peek() {
                    let done = ('\u{40}'..='\u{7e}').contains(&n);
                    chars.next();
                    if done {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: until BEL (0x7) or ST (ESC \).
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if n == '\u{7}' {
                        break;
                    }
                    if prev == '\u{1b}' && n == '\\' {
                        break;
                    }
                    prev = n;
                }
            }
            Some(_) => {} // ESC + single char (e.g. cursor ESC M): drop both
            None => {}    // dangling ESC: drop
        }
    }
    out
}

/// Split `line` into `(text, is_match)` segments for `needle`
/// (ASCII-case-insensitive). ASCII lowering preserves byte offsets, so the
/// indices stay valid for the original string.
pub fn split_highlight<'a>(line: &'a str, needle: &str) -> Vec<(&'a str, bool)> {
    if needle.is_empty() {
        return vec![(line, false)];
    }
    let hay = line.to_ascii_lowercase();
    let pat = needle.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(i) = hay[pos..].find(&pat) {
        let start = pos + i;
        if start > pos {
            out.push((&line[pos..start], false));
        }
        out.push((&line[start..start + pat.len()], true));
        pos = start + pat.len();
    }
    if pos < line.len() {
        out.push((&line[pos..], false));
    }
    out
}

/// `s` truncated to at most `max` chars (helper for draw).
fn truncate_to(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Tail-following log viewport . Vertical bar id is owned by the
/// window; the view is inserted and brokers bar events (ListViewer pattern).
pub struct LogView {
    scroller: Scroller,
    lines: VecDeque<String>,
    capacity: usize,
    follow: bool,
    highlight: Option<String>,
    v_bar: Option<ViewId>,
    /// Mouse-drag endpoints into the display lines.
    selection: TextSelection,
    /// Absolute screen origin cached by the last `draw`, used only as the
    /// capture origin for `start_mouse_track` (the router localizes events).
    abs_origin: Point,
    tracking: bool,
}

impl LogView {
    /// Ring-buffer cap .
    pub const DEFAULT_CAPACITY: usize = 10_000;

    pub fn new(bounds: Rect, v_bar: Option<ViewId>) -> Self {
        let mut scroller = Scroller::new(bounds, None, v_bar);
        // Interactive: `Options::default()` is selectable:false (decorative).
        scroller.state_mut().options = Options {
            selectable: true,
            first_click: true,
            ..Options::default()
        };
        Self {
            scroller,
            lines: VecDeque::new(),
            capacity: Self::DEFAULT_CAPACITY,
            follow: true,
            highlight: None,
            v_bar,
            selection: TextSelection::default(),
            abs_origin: Point::new(0, 0),
            tracking: false,
        }
    }

    /// Append one line, enforcing both caps (lines + bytes).
    pub fn push_line(&mut self, line: impl Into<String>) {
        let display = truncate_line(&strip_ansi(&line.into()));
        self.lines.push_back(display);
        while self.lines.len() > self.capacity {
            self.lines.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
    pub fn line(&self, i: usize) -> Option<&str> {
        self.lines.get(i).map(String::as_str)
    }
    pub fn follows(&self) -> bool {
        self.follow
    }
    pub fn set_follow(&mut self, follow: bool) {
        self.follow = follow;
    }
    pub fn scroll_offset(&self) -> i32 {
        self.scroller.delta.y
    }
    pub fn set_highlight(&mut self, needle: Option<String>) {
        self.highlight = needle;
    }

    /// Drop every line (Details rebuilds; a fresh Logs session syncs
    /// into an empty view instead — Task 6).
    pub fn clear(&mut self) {
        self.lines.clear();
        self.selection.clear();
        self.tracking = false;
    }

    /// Mouse positions arrive already localized to this view (both
    /// `Group::deliver` and the mouse-track capture subtract the origin), so
    /// the hit test uses a zero origin. The column is snapped to a grapheme
    /// boundary of the *displayed* line before it becomes a byte offset.
    fn hit(&self, position: Point) -> Option<TextPos> {
        let mut pos = super::selection::hit_test(
            Point::new(0, 0),
            position,
            self.scroll_offset(),
            self.lines.len(),
        )?;
        if let Some(line) = self.lines.get(pos.line) {
            let column = pos.byte.min(i32::MAX as usize) as i32;
            pos.byte = super::selection::column_to_byte(line, column);
        }
        Some(pos)
    }

    /// Whether a mouse selection is active (the window's Esc chain needs
    /// this to clear it before it closes the window).
    pub fn has_selection(&self) -> bool {
        !self.selection.is_empty()
    }

    /// Drop just the selection — not the lines. Re-publishes the command
    /// policy so Copy grays out again.
    pub fn clear_selection(&mut self, ctx: &mut Context) {
        if !self.selection.is_empty() {
            self.selection.clear();
            self.update_commands(ctx);
        }
    }

    /// Gray the Edit menu from the focused view's current selection state.
    fn update_commands(&self, ctx: &mut Context) {
        if self.selection.is_empty() {
            ctx.disable_command(Command::COPY);
        } else {
            ctx.enable_command(Command::COPY);
        }
        ctx.disable_command(Command::CUT);
        ctx.disable_command(Command::PASTE);
    }

    fn copy_selection(&mut self, ctx: &mut Context) {
        let text = self
            .selection
            .selected_text(self.lines.iter().map(String::as_str));
        if text.is_empty() {
            return;
        }
        ctx.set_clipboard(text.clone());
        crate::clipboard::put(&text);
        self.selection.clear();
        self.update_commands(ctx);
    }
    /// How many stored lines contain `needle` (ASCII-case-insensitive).
    pub fn count_matching(&self, needle: &str) -> usize {
        if needle.is_empty() {
            return self.lines.len();
        }
        let pat = needle.to_ascii_lowercase();
        self.lines
            .iter()
            .filter(|l| l.to_ascii_lowercase().contains(&pat))
            .count()
    }

    fn visible(&self) -> usize {
        self.scroller.state().size.y.max(0) as usize
    }

    /// Publish the limit; when following, jump to the tail .
    pub fn sync(&mut self, ctx: &mut Context) {
        self.scroller.set_limit(0, self.lines.len() as i32, ctx);
        if self.follow {
            let off = (self.lines.len() as i32 - self.visible() as i32).max(0);
            self.apply_offset(off, ctx);
        }
    }

    fn apply_offset(&mut self, want: i32, ctx: &mut Context) {
        let max = (self.scroller.limit().y - self.visible() as i32).max(0);
        let want = want.clamp(0, max);
        self.scroller.apply_delta(Point::new(0, want));
        self.scroller.scroll_to(0, want, ctx);
    }

    fn scroll_by(&mut self, delta: i32, ctx: &mut Context) {
        self.follow = false;
        self.apply_offset(self.scroller.delta.y + delta, ctx);
    }
}

#[delegate(to = scroller)]
impl View for LogView {
    fn draw(&mut self, ctx: &mut DrawCtx) {
        // Cache the absolute origin for the mouse-track capture. Mouse events
        // are delivered view-local, so hit_test does NOT subtract this again.
        self.abs_origin = ctx.origin();
        let normal = ctx.style(Role::ScrollerNormal);
        ctx.fill(self.scroller.state().get_extent(), ' ', normal);
        let w = self.scroller.state().size.x.max(0) as usize;
        let vis = self.visible();
        let off = self.scroller.delta.y.max(0) as usize;
        let selected = ctx.style(Role::ScrollerSelected);
        let needle = self.highlight.as_deref().unwrap_or("");
        for (i, line) in self.lines.iter().skip(off).take(vis).enumerate() {
            let shown = truncate_to(line, w);
            // Split at the selection endpoints first: the selected spans win
            // over search highlighting (both paint ScrollerSelected).
            let mut x = 0usize;
            for (a, b, in_selection) in self.selection.segments_for_line(off + i, &shown) {
                let piece = &shown[a..b];
                if piece.is_empty() {
                    continue;
                }
                if in_selection {
                    ctx.put_str(x as i32, i as i32, piece, selected);
                    x += tv::text::width(piece);
                } else {
                    for (frag, is_match) in split_highlight(piece, needle) {
                        if frag.is_empty() {
                            continue;
                        }
                        let style = if is_match { selected } else { normal };
                        ctx.put_str(x as i32, i as i32, frag, style);
                        x += tv::text::width(frag);
                    }
                }
            }
        }
    }

    fn set_state(&mut self, flag: StateFlag, enable: bool, ctx: &mut Context) {
        self.scroller.set_state(flag, enable, ctx);
        if flag != StateFlag::Focused {
            return;
        }
        if enable {
            self.update_commands(ctx);
        } else {
            ctx.disable_command(Command::COPY);
            ctx.disable_command(Command::CUT);
            ctx.disable_command(Command::PASTE);
        }
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        // Bar → content: the embedded Scroller has no ViewId, so this
        // inserted composite brokers the sync (ListViewer pattern).
        if let Event::Broadcast { command, source } = &*ev {
            let own = source.is_some() && *source == self.v_bar;
            if own && *command == Command::SCROLL_BAR_CLICKED {
                if let Some(id) = self.scroller.state().id() {
                    ctx.request_focus(id);
                }
            } else if own
                && *command == Command::SCROLL_BAR_CHANGED
                && let Some(id) = self.scroller.state().id()
            {
                ctx.request_scroll_sync(id, None, self.v_bar);
            }
            return;
        }
        match ev {
            Event::Command(Command::COPY) => {
                self.copy_selection(ctx);
                ev.clear();
            }
            Event::Command(Command::CUT | Command::PASTE) => {
                // Read-only view: never mutate the buffer, never consume the
                // system clipboard into it.
                ev.clear();
            }
            Event::MouseDown(m) if m.buttons.left => {
                if let Some(pos) = self.hit(m.position) {
                    self.selection.begin(pos);
                    self.tracking = true;
                    self.follow = false;
                    if let Some(id) = self.scroller.state().id() {
                        ctx.start_mouse_track(
                            id,
                            self.abs_origin,
                            TrackMask {
                                mouse_move: true,
                                mouse_auto: true,
                                ..Default::default()
                            },
                        );
                    }
                }
                ev.clear();
            }
            Event::MouseMove(m) | Event::MouseAuto(m) if self.tracking => {
                if let Some(pos) = self.hit(m.position) {
                    self.selection.extend(pos);
                }
                ev.clear();
            }
            Event::MouseUp(m) if self.tracking => {
                if let Some(pos) = self.hit(m.position) {
                    self.selection.extend(pos);
                }
                self.tracking = false;
                let text = self
                    .selection
                    .selected_text(self.lines.iter().map(String::as_str));
                if !text.is_empty() {
                    ctx.set_primary(text);
                }
                self.update_commands(ctx);
                ev.clear();
            }
            Event::KeyDown(ke) => {
                let key = ke.key;
                let mods = ke.modifiers;
                match key {
                    Key::Esc if !self.selection.is_empty() => {
                        self.selection.clear();
                        self.update_commands(ctx);
                        ev.clear();
                    }
                    Key::Char('c') if mods.ctrl => {
                        ctx.post(Command::COPY);
                        ev.clear();
                    }
                    Key::Insert if mods.ctrl || mods.shift => {
                        ctx.post(Command::COPY);
                        ev.clear();
                    }
                    Key::Delete if mods.shift => {
                        ctx.post(Command::COPY);
                        ev.clear();
                    }
                    Key::Up => {
                        self.scroll_by(-1, ctx);
                        ev.clear();
                    }
                    Key::Down => {
                        self.scroll_by(1, ctx);
                        ev.clear();
                    }
                    Key::PageUp => {
                        self.scroll_by(-(self.visible().max(1) as i32), ctx);
                        ev.clear();
                    }
                    Key::PageDown => {
                        self.scroll_by(self.visible().max(1) as i32, ctx);
                        ev.clear();
                    }
                    Key::Home => {
                        self.follow = false;
                        self.apply_offset(0, ctx);
                        ev.clear();
                    }
                    Key::End => {
                        self.follow = true;
                        self.sync(ctx);
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
            _ => {}
        }
    }

    fn change_bounds(&mut self, bounds: Rect) {
        self.scroller.change_bounds(bounds);
    }

    fn on_bounds_changed(&mut self, ctx: &mut Context) {
        // Resize while following sticks to the tail; otherwise clamp.
        if self.follow {
            self.sync(ctx);
        } else {
            self.scroller.set_limit(0, self.lines.len() as i32, ctx);
            let keep = self.scroller.delta.y;
            self.apply_offset(keep, ctx);
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{render_view, send_key, styled_text, text, with_ctx_io};
    use tv::event::{KeyEvent, KeyModifiers, MouseButtons, MouseEvent};
    use tv::{Deferred, Theme};

    fn view(h: i32) -> LogView {
        LogView::new(Rect::new(0, 0, 30, h), None)
    }

    fn mouse_at(position: Point) -> MouseEvent {
        MouseEvent {
            position,
            buttons: MouseButtons {
                left: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn send(log: &mut LogView, ev: Event, ctx: &mut Context) {
        let mut ev = ev;
        log.handle_event(&mut ev, ctx);
    }

    /// A left press at `from`, a move to `to`, then a release — the real
    /// capture would deliver the last two localized by `abs_origin`.
    fn drag(log: &mut LogView, from: Point, to: Point) -> Vec<Deferred> {
        let (_, deferred) = with_ctx_io(|ctx| {
            send(log, Event::MouseDown(mouse_at(from)), ctx);
            send(log, Event::MouseMove(mouse_at(to)), ctx);
            send(log, Event::MouseUp(mouse_at(to)), ctx);
        });
        deferred
    }

    fn sample(log: &mut LogView) {
        log.abs_origin = Point::new(0, 0);
        for line in ["header", "abc def", "third", "fourth", "fifth"] {
            log.push_line(line);
        }
    }

    fn selected(log: &LogView) -> String {
        log.selection
            .selected_text(log.lines.iter().map(String::as_str))
    }

    #[test]
    fn push_populates_lines_in_order() {
        let mut log = view(4);
        log.push_line("one");
        log.push_line("two");
        assert_eq!(log.len(), 2);
        assert_eq!(log.line(0), Some("one"));
        assert_eq!(log.line(1), Some("two"));
        assert!(log.follows(), "follow defaults to on");
    }

    #[test]
    fn capacity_cap_drops_the_oldest_line() {
        let mut log = view(4);
        for i in 0..(LogView::DEFAULT_CAPACITY + 5) {
            log.push_line(format!("L{i}"));
        }
        assert_eq!(log.len(), LogView::DEFAULT_CAPACITY, "hard 10k cap");
        assert_eq!(log.line(0), Some("L5"), "oldest lines evicted");
    }

    #[test]
    fn long_lines_are_truncated_to_the_byte_cap() {
        let mut log = view(4);
        log.push_line("a".repeat(5_000));
        let stored = log.line(0).unwrap();
        assert_eq!(stored.len(), MAX_LINE_BYTES, "≤ 2048 bytes stored");
        assert!(stored.ends_with('…'), "marker present");
        // Multi-byte safety: cut must land on a char boundary.
        let mut log = view(4);
        log.push_line("é".repeat(4_000)); // 2 bytes each
        let stored = log.line(0).unwrap();
        assert!(stored.len() <= MAX_LINE_BYTES);
        assert!(stored.ends_with('…'));
        assert!(stored.is_char_boundary(stored.len() - '…'.len_utf8()));
    }

    #[test]
    fn follow_scrolls_to_the_bottom_on_sync() {
        let mut log = view(4); // visible = 4
        for i in 0..10 {
            log.push_line(format!("L{i}"));
        }
        crate::testutil::with_ctx(|ctx| log.sync(ctx));
        assert_eq!(log.scroll_offset(), 6, "shows L6..L9");
    }

    #[test]
    fn manual_scroll_disables_follow_until_end() {
        let mut log = view(4);
        for i in 0..10 {
            log.push_line(format!("L{i}"));
        }
        crate::testutil::with_ctx(|ctx| log.sync(ctx));
        send_key(&mut log, Key::Up);
        assert_eq!(log.scroll_offset(), 5, "one line back");
        assert!(!log.follows(), "manual scroll pauses follow");
        // Push more lines; sync must NOT jump while un-followed.
        for i in 10..14 {
            log.push_line(format!("L{i}"));
        }
        crate::testutil::with_ctx(|ctx| log.sync(ctx));
        assert_eq!(log.scroll_offset(), 5, "view stays put");
        send_key(&mut log, Key::End);
        assert!(log.follows(), "End resumes follow");
        assert_eq!(log.scroll_offset(), 10, "jumped to bottom (14 − 4)");
    }

    #[test]
    fn home_scrolls_to_the_top() {
        let mut log = view(3);
        for i in 0..10 {
            log.push_line(format!("L{i}"));
        }
        crate::testutil::with_ctx(|ctx| log.sync(ctx));
        send_key(&mut log, Key::Home);
        assert_eq!(log.scroll_offset(), 0);
    }

    #[test]
    fn split_highlight_segments_matches_case_insensitively() {
        let segs = split_highlight("a ERR b err", "ERR");
        assert_eq!(
            segs,
            vec![("a ", false), ("ERR", true), (" b ", false), ("err", true)]
        );
        assert_eq!(split_highlight("clean", "ERR"), vec![("clean", false)]);
        assert_eq!(split_highlight("anything", ""), vec![("anything", false)]);
    }

    #[test]
    fn count_matching_lines() {
        let mut log = view(4);
        log.push_line("INFO ok");
        log.push_line("ERROR boom");
        log.push_line("error again");
        assert_eq!(log.count_matching("error"), 2);
        assert_eq!(log.count_matching(""), 3);
    }

    #[test]
    fn draw_shows_the_tail_while_following() {
        let mut log = view(3);
        for i in 0..5 {
            log.push_line(format!("tail-{i}"));
        }
        crate::testutil::with_ctx(|ctx| log.sync(ctx));
        let out = text(&render_view(30, 3, &mut log));
        assert!(out.contains("tail-4"), "bottom visible: {out:?}");
        assert!(!out.contains("tail-0"), "scrolled past the top: {out:?}");
    }

    #[test]
    fn draw_renders_highlighted_segments_without_panicking() {
        let mut log = view(3);
        log.push_line("ERROR first");
        log.push_line("ok second");
        log.set_highlight(Some("ERROR".into()));
        crate::testutil::with_ctx(|ctx| log.sync(ctx));
        let out = text(&render_view(30, 3, &mut log));
        assert!(out.contains("ERROR"), "highlight keeps the text: {out:?}");
    }

    #[test]
    fn draw_degenerate_sizes_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (30, 1), (1, 30)] {
            let mut log = LogView::new(Rect::new(0, 0, w, h), None);
            log.push_line("x");
            let screen = render_view(8, 8, &mut log);
            let _ = text(&screen);
        }
    }

    #[test]
    fn as_any_mut_downcasts_to_log_view() {
        let mut log = view(4);
        let view: &mut dyn View = &mut log;
        assert!(
            view.as_any_mut()
                .is_some_and(|a| a.downcast_ref::<LogView>().is_some())
        );
    }

    #[test]
    fn strip_ansi_removes_csi_and_osc_sequences_only() {
        // CSI (colors), OSC (window title), single-char ESC — the shapes
        // docker output actually carries.
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(
            strip_ansi("\u{1b}]0;title\u{7}text"),
            "text",
            "OSC ends at BEL"
        );
        assert_eq!(
            strip_ansi("\u{1b}]0;title\u{1b}\\text"),
            "text",
            "OSC ends at ST (ESC \\)"
        );
        assert_eq!(
            strip_ansi("plain 日本語"),
            "plain 日本語",
            "no escapes: same text"
        );
        assert_eq!(
            strip_ansi("tail\u{1b}"),
            "tail",
            "dangling ESC at the end is dropped"
        );
    }

    #[test]
    fn strip_ansi_then_truncate_handles_ansi_unicode_and_the_cap() {
        // Display text only, byte cap AFTER stripping, Unicode + ANSI +
        // truncation in one line.
        let ansi = "\u{1b}[1;32m";
        let reset = "\u{1b}[0m";
        let wide = "日本語テキスト".repeat(400); // 12 × 400 = 4800 chars ≈ 14400 bytes
        let raw = format!("{ansi}{wide}{reset}");
        assert!(raw.len() > MAX_LINE_BYTES, "raw exceeds the cap");
        let mut view = view(5);
        view.push_line(raw.clone());
        let stored = view.line(0).expect("one line");
        assert!(!stored.contains('\u{1b}'), "stored holds display text only");
        assert!(
            stored.len() <= MAX_LINE_BYTES,
            "cap applied AFTER stripping: {} bytes",
            stored.len()
        );
        assert!(stored.starts_with("日本語"), "Unicode boundary kept");
        assert!(stored.ends_with('…'), "truncation marker present");
        // Stripping first means ANSI bytes never eat the budget: the
        // stripped text is far longer than what a strip-after-cap would keep.
        assert!(
            strip_ansi(&raw).len() > MAX_LINE_BYTES,
            "pre-strip length justifies the order"
        );
    }

    #[test]
    fn clear_empties_the_buffer_for_rebuilds() {
        let mut view = view(4);
        view.push_line("one");
        view.push_line("two");
        view.clear();
        assert_eq!(view.len(), 0);
        assert!(view.is_empty());
    }

    #[test]
    fn dragging_selects_exact_characters_and_publishes_primary() {
        let mut log = view(5);
        sample(&mut log);
        let deferred = drag(&mut log, Point::new(2, 1), Point::new(6, 1));
        assert_eq!(
            selected(&log),
            "c de",
            "exact display characters, not whole lines"
        );
        assert!(!log.follows(), "a mouse selection pauses tail-follow");
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::SetPrimary(s) if s == "c de")),
            "mouse selection is published as PRIMARY"
        );
    }

    #[test]
    fn reverse_drag_and_copy_use_visual_order_then_clear() {
        let mut log = view(5);
        sample(&mut log);
        drag(&mut log, Point::new(6, 1), Point::new(2, 1));
        assert_eq!(
            selected(&log),
            "c de",
            "a backwards drag selects forward text"
        );

        let (events, _) = with_ctx_io(|ctx| {
            let mut ev = Event::KeyDown(KeyEvent::new(
                Key::Char('c'),
                KeyModifiers {
                    ctrl: true,
                    ..Default::default()
                },
            ));
            log.handle_event(&mut ev, ctx);
            assert!(ev.is_nothing(), "Ctrl+C is consumed in a read-only view");
        });
        assert!(
            matches!(events.as_slice(), [Event::Command(Command::COPY)]),
            "the chord is translated to a command: {events:?}"
        );

        let (_, deferred) = with_ctx_io(|ctx| {
            send(&mut log, Event::Command(Command::COPY), ctx);
        });
        assert_eq!(crate::clipboard::text().as_deref(), Some("c de"));
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::SetClipboard(s) if s == "c de")),
            "the OS/OSC-52 route gets the same text"
        );
        assert!(log.selection.is_empty(), "copy consumes the selection");
    }

    #[test]
    fn esc_and_clear_discard_a_selection() {
        let mut log = view(5);
        sample(&mut log);
        drag(&mut log, Point::new(2, 1), Point::new(6, 1));
        assert!(!log.selection.is_empty());
        with_ctx_io(|ctx| {
            send(
                &mut log,
                Event::KeyDown(KeyEvent::new(Key::Esc, KeyModifiers::default())),
                ctx,
            );
        });
        assert!(log.selection.is_empty(), "Esc clears an active selection");

        drag(&mut log, Point::new(2, 1), Point::new(6, 1));
        log.clear();
        assert!(log.selection.is_empty(), "clear() clears a selection too");
    }

    #[test]
    fn selected_cells_use_the_scroller_selected_role() {
        let mut log = view(3);
        sample(&mut log);
        drag(&mut log, Point::new(2, 1), Point::new(6, 1));
        let cells = styled_text(&render_view(30, 3, &mut log));
        let theme = Theme::classic_blue();
        // The drag is on screen row 1 ("abc def"), so cell index = 30 + column.
        assert_eq!(
            cells[30 + 1].1,
            theme.style(Role::ScrollerNormal),
            "cell 'b' is normal"
        );
        assert_eq!(
            cells[30 + 2].1,
            theme.style(Role::ScrollerSelected),
            "cell 'c' is selected"
        );
        assert_eq!(
            cells[30 + 5].1,
            theme.style(Role::ScrollerSelected),
            "cell 'e' is selected"
        );
        assert_eq!(
            cells[30 + 6].1,
            theme.style(Role::ScrollerNormal),
            "cell 'f' is normal"
        );
    }

    #[test]
    fn cut_and_paste_commands_are_no_ops_in_a_read_only_view() {
        let mut log = view(5);
        sample(&mut log);
        drag(&mut log, Point::new(2, 1), Point::new(6, 1));
        with_ctx_io(|ctx| {
            send(&mut log, Event::Command(Command::CUT), ctx);
            send(&mut log, Event::Command(Command::PASTE), ctx);
        });
        assert_eq!(selected(&log), "c de", "read-only view keeps its selection");
    }
}
