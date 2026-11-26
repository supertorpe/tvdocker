#![allow(dead_code)]
//! Headless harness for unit-testing views without a `Program` :
//! drive events through a real `Context`, render through a real `Renderer`.

use std::collections::VecDeque;
use tv::event::{Event, Key, KeyEvent, KeyModifiers};
use tv::{
    Buffer, Context, Deferred, DrawCtx, HeadlessBackend, HeadlessHandle, Point, Rect, Renderer,
    Style, Theme, TimerQueue, View,
};

/// Run `f` against a real (empty-output) context.
pub fn with_ctx(f: impl FnOnce(&mut Context)) {
    let mut out: VecDeque<Event> = VecDeque::new();
    let mut timers = TimerQueue::new();
    let mut deferred: Vec<Deferred> = Vec::new();
    let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
    f(&mut ctx);
}

/// Like [`with_ctx`], but return the deferred operations it queued (command
/// enable/disable, clipboard writes, timer installs, …) for assertions.
pub fn with_ctx_d(f: impl FnOnce(&mut Context)) -> Vec<Deferred> {
    let mut out: VecDeque<Event> = VecDeque::new();
    let mut timers = TimerQueue::new();
    let mut deferred: Vec<Deferred> = Vec::new();
    let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
    f(&mut ctx);
    deferred
}

/// Return both the posted-event queue and the deferred operations produced by
/// `f`, so a test can assert a chord was translated into a command AND that a
/// command changed the clipboard state.
pub fn with_ctx_io(f: impl FnOnce(&mut Context)) -> (Vec<Event>, Vec<Deferred>) {
    let mut out: VecDeque<Event> = VecDeque::new();
    let mut timers = TimerQueue::new();
    let mut deferred: Vec<Deferred> = Vec::new();
    let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
    f(&mut ctx);
    (out.into_iter().collect(), deferred)
}

/// Deliver a key with default modifiers; clears only if the view handles it.
pub fn send_key(view: &mut dyn View, key: Key) {
    send_key_mods(view, key, KeyModifiers::default());
}

pub fn send_key_mods(view: &mut dyn View, key: Key, mods: KeyModifiers) {
    with_ctx(|ctx| {
        let mut ev = Event::KeyDown(KeyEvent::new(key, mods));
        view.handle_event(&mut ev, ctx);
    });
}

pub fn send_event(view: &mut dyn View, mut ev: Event) {
    with_ctx(|ctx| view.handle_event(&mut ev, ctx));
}

/// Draw `view` onto a fresh headless screen; returns the handle.
pub fn render_view(w: u16, h: u16, view: &mut dyn View) -> HeadlessHandle {
    let (backend, screen) = HeadlessBackend::new(w, h);
    let theme = Theme::classic_blue();
    let mut r = Renderer::new(Box::new(backend));
    r.render(|buf: &mut Buffer| {
        let mut ctx = DrawCtx::new(
            buf,
            &theme,
            Rect::new(0, 0, i32::from(w), i32::from(h)),
            Point::new(0, 0),
        );
        view.draw(&mut ctx);
    });
    screen
}

/// Headless screen as one string (row-per-line).
pub fn text(handle: &HeadlessHandle) -> String {
    let buf = handle.buffer();
    let mut out = String::new();
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            out.push_str(buf.get(x, y).symbol());
        }
        out.push('\n');
    }
    out
}

/// Headless screen as `(symbol, style)` per cell — lets a test assert **which
/// role** painted a cell, not just that some character appeared there.
pub fn styled_text(handle: &HeadlessHandle) -> Vec<(String, Style)> {
    let buf = handle.buffer();
    let mut out = Vec::new();
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            let cell = buf.get(x, y);
            out.push((cell.symbol().to_owned(), cell.style()));
        }
    }
    out
}

/// The popup menus a deferred batch opened (`Deferred::OpenMenuBox`) —
/// how a test asserts that a header right-click opened the table's
/// column popup, or a body right-click the row context menu.
pub fn menus_opened(deferred: &[Deferred]) -> Vec<tv::menu::Menu> {
    deferred
        .iter()
        .filter_map(|d| match d {
            Deferred::OpenMenuBox { menu, .. } => Some(menu.clone()),
            _ => None,
        })
        .collect()
}

/// Deliver one event through a real `Context` and return the popup menus it
/// opened. Shared by context-menu tests across the window modules.
pub fn menus_from_event(view: &mut dyn View, ev: Event) -> Vec<tv::Menu> {
    let mut out: VecDeque<Event> = VecDeque::new();
    let mut timers = TimerQueue::new();
    let mut deferred: Vec<Deferred> = Vec::new();
    {
        let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
        let mut ev = ev;
        view.handle_event(&mut ev, &mut ctx);
    }
    menus_opened(&deferred)
}

/// A menu's item labels, `~` hot markers stripped.
pub fn menu_labels(menu: &tv::menu::Menu) -> Vec<String> {
    menu.items
        .iter()
        .map(|i| match i {
            tv::menu::MenuItem::Command { name, .. } => name.replace('~', ""),
            _ => String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tv::{Role, ViewState};

    #[test]
    fn with_ctx_io_reports_posted_events_and_clipboard_ops() {
        let (events, deferred) = with_ctx_io(|ctx| {
            ctx.post(tv::Command::COPY);
            ctx.set_clipboard("x".into());
            ctx.set_primary("y".into());
        });
        assert!(
            matches!(events.as_slice(), [Event::Command(tv::Command::COPY)]),
            "a posted command lands in the out queue: {events:?}"
        );
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::SetClipboard(s) if s == "x")),
            "SetClipboard is deferred"
        );
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::SetPrimary(s) if s == "y")),
            "SetPrimary is deferred"
        );
    }

    #[test]
    fn with_ctx_d_reports_command_state_changes() {
        let deferred = with_ctx_d(|ctx| {
            ctx.enable_command(tv::Command::COPY);
            ctx.disable_command(tv::Command::PASTE);
        });
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::EnableCommand(tv::Command::COPY))),
            "command enable is deferred"
        );
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, Deferred::DisableCommand(tv::Command::PASTE))),
            "command disable is deferred"
        );
    }

    struct Painted {
        st: ViewState,
    }
    impl View for Painted {
        fn state(&self) -> &ViewState {
            &self.st
        }
        fn state_mut(&mut self) -> &mut ViewState {
            &mut self.st
        }
        fn draw(&mut self, ctx: &mut DrawCtx) {
            let style = ctx.style(Role::ScrollerSelected);
            ctx.put_str(2, 3, "Hi", style);
        }
    }

    #[test]
    fn styled_text_exposes_the_role_that_painted_a_cell() {
        let handle = render_view(
            10,
            6,
            &mut Painted {
                st: ViewState::new(Rect::new(0, 0, 10, 6)),
            },
        );
        let cells = styled_text(&handle);
        assert_eq!(cells[3 * 10 + 2].0, "H");
        assert_eq!(
            cells[3 * 10 + 2].1,
            Theme::classic_blue().style(Role::ScrollerSelected),
            "the cell carries the selected role's style"
        );
    }
}
