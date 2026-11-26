//! A read-only, scrollable text pane: a stripped
//! `LogView` — a `Scroller`-backed viewport over shared lines (the window
//! is the mutator). Each line is styled by `severity_role` when `severity`
//! is on, else uniformly (`Role::ListNormal`). No scrollbar — the
//! Splitter's panes are the scroll surfaces (ruling 3).
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key, MouseWheel};
use tv::{
    Command, Context, DrawCtx, Options, Point, Rect, Role, Scroller, StateFlag, TrackMask, View,
    ViewState,
};

use super::selection::{TextPos, TextSelection};

/// Map one analysis line to a theme role. Ordering
/// matters: an error line that also says "ok" stays an error.
pub fn severity_role(line: &str) -> Role {
    let l = line.to_lowercase();
    if l.contains("error") || l.contains("fatal") || l.contains("critical") || l.contains("panic") {
        Role::Error
    } else if l.contains("warn") {
        Role::Warning
    } else if l.contains("healthy") || l.contains(" ok") || l.starts_with("ok") {
        Role::Success
    } else if l.trim_start().starts_with("step") || l.contains("step ") || l.contains("playbook") {
        Role::Info
    } else {
        Role::ListNormal
    }
}

/// A read-only viewport over shared lines. `severity` selects per-line
/// `severity_role` styling; otherwise every line uses `Role::ListNormal`.
pub struct TextPane {
    st: ViewState,
    /// Carries the scroll offset (`Scroller`-backed); no bars.
    scroller: Scroller,
    lines: Rc<RefCell<Vec<String>>>,
    severity: bool,
    /// Mouse-drag endpoints into the shared display lines.
    selection: TextSelection,
    /// Absolute screen origin cached by the last `draw`, used only as the
    /// capture origin for `start_mouse_track` (the router localizes events).
    abs_origin: Point,
    tracking: bool,
}

impl TextPane {
    /// Build a pane over `lines`. The `Rc<RefCell<..>>` is shared with the
    /// window, which mutates it; the pane reads it at draw time.
    pub fn new(bounds: Rect, lines: Rc<RefCell<Vec<String>>>, severity: bool) -> Self {
        let mut st = ViewState::new(bounds);
        st.options = Options {
            selectable: true,
            first_click: true,
            ..Options::default()
        };
        Self {
            st,
            scroller: Scroller::new(bounds, None, None),
            lines,
            severity,
            selection: TextSelection::default(),
            abs_origin: Point::new(0, 0),
            tracking: false,
        }
    }

    /// Drop a selection whose endpoints no longer refer to the displayed
    /// content (a wholesale line replacement by the owning window).
    pub fn invalidate_selection(&mut self) {
        self.selection.clear();
        self.tracking = false;
    }

    /// Mouse positions arrive already localized to this view (both
    /// `Group::deliver` and the mouse-track capture subtract the origin), so
    /// the hit test uses a zero origin. The column is snapped to a grapheme
    /// boundary of the displayed line before it becomes a byte offset.
    fn hit(&self, position: Point) -> Option<TextPos> {
        let lines = self.lines.borrow();
        let mut pos = super::selection::hit_test(
            Point::new(0, 0),
            position,
            self.scroll_offset(),
            lines.len(),
        )?;
        if let Some(line) = lines.get(pos.line) {
            let column = pos.byte.min(i32::MAX as usize) as i32;
            pos.byte = super::selection::column_to_byte(line, column);
        }
        Some(pos)
    }

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
            .selected_text(self.lines.borrow().iter().map(String::as_str));
        if text.is_empty() {
            return;
        }
        ctx.set_clipboard(text.clone());
        crate::clipboard::put(&text);
        self.selection.clear();
        self.update_commands(ctx);
    }

    /// Current scroll offset (first visible line) — the window's/tests'
    /// observation point.
    pub fn scroll_offset(&self) -> i32 {
        self.scroller.delta.y
    }

    /// Rows visible in the current bounds.
    fn visible(&self) -> i32 {
        self.st.size.y.max(0)
    }

    /// Republish the content length to the scroller (needs a `Context`,
    /// so it runs from key/resize handling).
    fn sync(&mut self, ctx: &mut Context) {
        self.scroller
            .set_limit(0, self.lines.borrow().len() as i32, ctx);
    }

    fn apply_offset(&mut self, want: i32, ctx: &mut Context) {
        let max = (self.scroller.limit().y - self.visible()).max(0);
        let want = want.clamp(0, max);
        self.scroller.apply_delta(Point::new(0, want));
        self.scroller.scroll_to(0, want, ctx);
    }

    fn scroll_by(&mut self, delta: i32, ctx: &mut Context) {
        self.sync(ctx);
        self.apply_offset(self.scroller.delta.y + delta, ctx);
    }
}

impl View for TextPane {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        // Cache the absolute origin for the mouse-track capture. Mouse events
        // are delivered view-local, so hit_test does NOT subtract this again.
        self.abs_origin = ctx.origin();
        let normal = ctx.style(Role::ListNormal);
        ctx.fill(self.st.get_extent(), ' ', normal);
        let off = self.scroller.delta.y.max(0) as usize;
        let selected = ctx.style(Role::ListSelected);
        let lines = self.lines.borrow();
        for (i, line) in lines
            .iter()
            .skip(off)
            .take(self.visible().max(0) as usize)
            .enumerate()
        {
            let style = if self.severity {
                ctx.style(severity_role(line))
            } else {
                normal
            };
            // Selection wins over severity styling on the selected spans.
            let mut x = 0usize;
            for (a, b, in_selection) in self.selection.segments_for_line(off + i, line) {
                let piece = &line[a..b];
                if piece.is_empty() {
                    continue;
                }
                let seg_style = if in_selection { selected } else { style };
                ctx.put_str(x as i32, i as i32, piece, seg_style);
                x += tv::text::width(piece);
            }
        }
    }

    /// Mirror PtyView: our own flags track focus (the hardware cursor is not
    /// relevant here, but broadcast state must still be correct), while the
    /// embedded Scroller gets every flag except `Focused`.
    fn set_state(&mut self, flag: StateFlag, enable: bool, ctx: &mut Context) {
        self.st.set_flag(flag, enable);
        if flag != StateFlag::Focused {
            self.scroller.set_state(flag, enable, ctx);
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
        match ev {
            Event::Command(Command::COPY) => {
                self.copy_selection(ctx);
                ev.clear();
            }
            Event::Command(Command::CUT | Command::PASTE) => {
                // Read-only pane: never mutate the lines or consume the
                // system clipboard into them.
                ev.clear();
            }
            Event::MouseDown(m) if m.buttons.left => {
                if let Some(pos) = self.hit(m.position) {
                    self.selection.begin(pos);
                    self.tracking = true;
                    if let Some(id) = self.st.id() {
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
                    .selected_text(self.lines.borrow().iter().map(String::as_str));
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
                        self.scroll_by(-(self.visible().max(1)), ctx);
                        ev.clear();
                    }
                    Key::PageDown => {
                        self.scroll_by(self.visible().max(1), ctx);
                        ev.clear();
                    }
                    Key::Home => {
                        self.sync(ctx);
                        self.apply_offset(0, ctx);
                        ev.clear();
                    }
                    Key::End => {
                        self.sync(ctx);
                        let max = (self.scroller.limit().y - self.visible()).max(0);
                        self.apply_offset(max, ctx);
                        ev.clear();
                    }
                    _ => {}
                }
            }
            Event::MouseWheel(m) => {
                let delta = match m.wheel {
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
        self.st.set_bounds(bounds);
        self.scroller.change_bounds(bounds);
    }

    fn on_bounds_changed(&mut self, ctx: &mut Context) {
        // Re-clamp on resize  — the limit moves with the height.
        self.sync(ctx);
        let keep = self.scroller.delta.y;
        self.apply_offset(keep, ctx);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{render_view, styled_text, with_ctx_d, with_ctx_io};
    use tv::event::{KeyEvent, KeyModifiers, MouseButtons, MouseEvent};
    use tv::{Command, Deferred, StateFlag, Theme};

    fn sample() -> (TextPane, Rc<RefCell<Vec<String>>>) {
        let lines = Rc::new(RefCell::new(vec![
            "header".to_string(),
            "abc def".to_string(),
            "third".to_string(),
            "fourth".to_string(),
            "fifth".to_string(),
        ]));
        let pane = TextPane::new(Rect::new(0, 0, 20, 5), Rc::clone(&lines), false);
        (pane, lines)
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

    fn drag(pane: &mut TextPane, from: Point, to: Point) -> Vec<Deferred> {
        let (_, deferred) = with_ctx_io(|ctx| {
            for ev in [
                Event::MouseDown(mouse_at(from)),
                Event::MouseMove(mouse_at(to)),
                Event::MouseUp(mouse_at(to)),
            ] {
                let mut ev = ev;
                pane.handle_event(&mut ev, ctx);
            }
        });
        deferred
    }

    fn selected(pane: &TextPane) -> String {
        pane.selection
            .selected_text(pane.lines.borrow().iter().map(String::as_str))
    }

    #[test]
    fn dragging_selects_exact_characters_and_publishes_primary() {
        let (mut pane, _) = sample();
        let deferred = drag(&mut pane, Point::new(2, 1), Point::new(6, 1));
        assert_eq!(selected(&pane), "c de", "exact characters, not whole lines");
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::SetPrimary(s) if s == "c de")),
            "mouse selection is published as PRIMARY"
        );
    }

    #[test]
    fn reverse_drag_and_copy_use_visual_order_then_clear() {
        let (mut pane, _) = sample();
        drag(&mut pane, Point::new(6, 1), Point::new(2, 1));
        assert_eq!(
            selected(&pane),
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
            pane.handle_event(&mut ev, ctx);
            assert!(ev.is_nothing(), "Ctrl+C is consumed in a read-only pane");
        });
        assert!(
            matches!(events.as_slice(), [Event::Command(Command::COPY)]),
            "the chord is translated to a command: {events:?}"
        );

        let (_, deferred) = with_ctx_io(|ctx| {
            let mut ev = Event::Command(Command::COPY);
            pane.handle_event(&mut ev, ctx);
        });
        assert_eq!(crate::clipboard::text().as_deref(), Some("c de"));
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::SetClipboard(s) if s == "c de")),
            "the OS/OSC-52 route gets the same text"
        );
        assert!(pane.selection.is_empty(), "copy consumes the selection");
    }

    #[test]
    fn esc_clear_and_invalidation_discard_a_selection() {
        let (mut pane, _) = sample();
        drag(&mut pane, Point::new(2, 1), Point::new(6, 1));
        assert!(!pane.selection.is_empty());
        with_ctx_io(|ctx| {
            let mut ev = Event::KeyDown(KeyEvent::new(Key::Esc, KeyModifiers::default()));
            pane.handle_event(&mut ev, ctx);
        });
        assert!(pane.selection.is_empty(), "Esc clears an active selection");

        drag(&mut pane, Point::new(2, 1), Point::new(6, 1));
        pane.invalidate_selection();
        assert!(
            pane.selection.is_empty(),
            "explicit invalidation clears it too"
        );
    }

    #[test]
    fn selected_cells_use_the_list_selected_role() {
        let (mut pane, _) = sample();
        drag(&mut pane, Point::new(2, 1), Point::new(6, 1));
        let cells = styled_text(&render_view(20, 5, &mut pane));
        let theme = Theme::classic_blue();
        // The drag is on screen row 1 ("abc def"), so index = 20 + column.
        assert_eq!(
            cells[20 + 1].1,
            theme.style(Role::ListNormal),
            "cell 'b' is normal"
        );
        assert_eq!(
            cells[20 + 2].1,
            theme.style(Role::ListSelected),
            "cell 'c' is selected"
        );
        assert_eq!(
            cells[20 + 5].1,
            theme.style(Role::ListSelected),
            "cell 'e' is selected"
        );
        assert_eq!(
            cells[20 + 6].1,
            theme.style(Role::ListNormal),
            "cell 'f' is normal"
        );
    }

    #[test]
    fn focus_gates_the_edit_commands() {
        let (mut pane, _) = sample();
        let deferred = with_ctx_d(|ctx| {
            pane.set_state(StateFlag::Focused, true, ctx);
        });
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::DisableCommand(Command::COPY))),
            "COPY is grayed on focus until characters are selected"
        );
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::DisableCommand(Command::CUT))),
            "CUT stays disabled in a read-only pane"
        );
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::DisableCommand(Command::PASTE))),
            "PASTE stays disabled in a read-only pane"
        );

        drag(&mut pane, Point::new(2, 1), Point::new(6, 1));
        let deferred = with_ctx_d(|ctx| {
            pane.set_state(StateFlag::Focused, true, ctx);
        });
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::EnableCommand(Command::COPY))),
            "a selection enables COPY while focused"
        );

        let deferred = with_ctx_d(|ctx| {
            pane.set_state(StateFlag::Focused, false, ctx);
        });
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::DisableCommand(Command::COPY))),
            "losing focus disables COPY"
        );
    }

    #[test]
    fn invalidating_after_a_wholesale_line_replacement_copies_nothing() {
        let lines = Rc::new(RefCell::new(vec![
            "header".to_string(),
            "abc def".to_string(),
            "third".to_string(),
        ]));
        let mut pane = TextPane::new(Rect::new(0, 0, 20, 4), Rc::clone(&lines), false);
        drag(&mut pane, Point::new(2, 1), Point::new(6, 1));
        assert_eq!(selected(&pane), "c de");

        // The window replaces the shared vector wholesale; the stale
        // endpoints must not copy text out of the new content.
        *lines.borrow_mut() = vec!["brand".to_string(), "new".to_string()];
        pane.invalidate_selection();
        with_ctx_io(|ctx| {
            let mut ev = Event::Command(Command::COPY);
            pane.handle_event(&mut ev, ctx);
        });
        assert!(
            !crate::clipboard::text()
                .as_deref()
                .is_some_and(|s| s.contains("c de")),
            "no stale text reaches the clipboard"
        );
    }

    /// Review Focus #2.
    #[test]
    fn severity_role_maps_severities() {
        assert_eq!(severity_role("ERROR: cannot start"), Role::Error);
        assert_eq!(severity_role("  fatal: boom"), Role::Error);
        assert_eq!(severity_role("ERROR: ok now"), Role::Error);
        assert_eq!(severity_role("warning: deprecated flag"), Role::Warning);
        assert_eq!(severity_role("WARN disk almost full"), Role::Warning);
        assert_eq!(severity_role("container is healthy"), Role::Success);
        assert_eq!(severity_role("OK"), Role::Success);
        assert_eq!(severity_role("Step 1: restart the container"), Role::Info);
        assert_eq!(severity_role("just some text"), Role::ListNormal);
    }

    #[test]
    fn scrolling_clamps_to_the_line_count() {
        let lines = Rc::new(RefCell::new(vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
        ]));
        let mut pane = TextPane::new(Rect::new(0, 0, 20, 2), Rc::clone(&lines), false);
        crate::testutil::with_ctx(|ctx| pane.sync(ctx));
        // Two rows tall, three lines: max offset is 1.
        crate::testutil::send_key(&mut pane, tv::Key::Down);
        crate::testutil::send_key(&mut pane, tv::Key::Down);
        assert_eq!(pane.scroll_offset(), 1, "clamped to len - visible");
        crate::testutil::send_key(&mut pane, tv::Key::Home);
        assert_eq!(pane.scroll_offset(), 0);
        // Content grows and End reaches the new tail.
        lines.borrow_mut().push("d".to_string());
        crate::testutil::send_key(&mut pane, tv::Key::End);
        assert_eq!(pane.scroll_offset(), 2);
    }
}
