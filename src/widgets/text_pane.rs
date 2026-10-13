//! A read-only, scrollable text pane: a stripped
//! `LogView` — a `Scroller`-backed viewport over shared lines (the window
//! is the mutator). Each line is styled by `severity_role` when `severity`
//! is on, else uniformly (`Role::ListNormal`). No scrollbar — the
//! Splitter's panes are the scroll surfaces (ruling 3).
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key, MouseWheel};
use tv::{Context, DrawCtx, Options, Point, Rect, Role, Scroller, View, ViewState};

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
        }
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
        let normal = ctx.style(Role::ListNormal);
        ctx.fill(self.st.get_extent(), ' ', normal);
        let off = self.scroller.delta.y.max(0) as usize;
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
            ctx.put_str(0, i as i32, line, style);
        }
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        match ev {
            Event::KeyDown(ke) => {
                let key = ke.key;
                match key {
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
    use tv::Role;

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
