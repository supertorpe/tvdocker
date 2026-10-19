#![allow(dead_code)]
//! Headless harness for unit-testing views without a `Program` :
//! drive events through a real `Context`, render through a real `Renderer`.

use std::collections::VecDeque;
use tv::event::{Event, Key, KeyEvent, KeyModifiers};
use tv::{
    Buffer, Context, Deferred, DrawCtx, HeadlessBackend, HeadlessHandle, Point, Rect, Renderer,
    Theme, TimerQueue, View,
};

/// Run `f` against a real (empty-output) context.
pub fn with_ctx(f: impl FnOnce(&mut Context)) {
    let mut out: VecDeque<Event> = VecDeque::new();
    let mut timers = TimerQueue::new();
    let mut deferred: Vec<Deferred> = Vec::new();
    let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
    f(&mut ctx);
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
