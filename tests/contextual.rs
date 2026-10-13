#![forbid(unsafe_code)]
//! Contextual status defs  through the REAL pump: the
//! `DynamicStatus` wrapper must forward the focused view's help context,
//! and the Ctrl+A chord must reach the window as `Event::Command`.
mod common;

use common::{container_state, pump, screen_text};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use tv::event::{Event, Key, KeyEvent, KeyModifiers};
use tv::{Context, Deferred, HeadlessHandle, Program, Rect, StatusLine, TimerQueue, View};
use tvdocker::app;
use tvdocker::core::{AppCore, AppMsg, Connection};

fn start() -> (Program, HeadlessHandle, Rc<RefCell<AppCore>>) {
    // Real build(): dynamic status + desktop + bridge + menu. The config
    // path is isolated inside `common::app_start`.
    let h = common::app_start(|_| {});
    (h.program, h.screen, h.core)
}

/// The status line owns the LAST screen row (`init_status_line` pins it
/// to `r.b.y - 1` — row 24 on 80×25).
fn status_row(screen: &HeadlessHandle) -> String {
    screen_text(screen)
        .lines()
        .nth(24)
        .unwrap_or_default()
        .to_string()
}

fn ctrl_a() -> KeyEvent {
    KeyEvent::new(
        Key::Char('a'),
        KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        },
    )
}

/// Hand `ev` to `line` exactly as the pump's pre-route does
/// (program.rs:2217: direct `handle_event` on the stored status id, the
/// LIVE event) and return what the line left behind.
fn pre_route(line: &mut StatusLine, mut ev: Event) -> Event {
    let mut out = VecDeque::new();
    let mut timers = TimerQueue::new();
    let mut deferred: Vec<Deferred> = Vec::new();
    let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
    line.handle_event(&mut ev, &mut ctx);
    ev
}

#[test]
fn contextual_status_shows_container_actions_and_hides_them_for_menus() {
    let (mut program, screen, _core) = start();
    // Startup: 5 pumps — the grant → COMMAND_SET_CHANGED → status-reprime
    // chain (chords precedent) — leaves Containers focused and the status
    // line primed with that window's help context.
    pump(&mut program, 5);
    // `Containers: F2 Logs · F4 Shell`
    // visible; the rest of the contextual bindings are hidden-but-bound
    // (their transforms are pinned by
    // app::tests::containers_context_transforms… and by the Ctrl+A
    // pre-route test in this file).
    let row = status_row(&screen);
    assert!(
        row.contains("F2 Logs") && row.contains("F4 Shell"),
        "example visible: {row}"
    );
    assert!(!row.contains('^'), "hidden bindings draw nothing: {row}");

    // F10 opens the menu ⇒ universal def. Enter is what actually opens
    // the File popup (the same two keys `chrome`'s F10 test uses), and
    // the popup is the sanity check that the menu really took focus.
    screen.push_key(Key::F(10), KeyModifiers::default());
    pump(&mut program, 5);
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 5);
    assert!(
        screen_text(&screen)
            .lines()
            .take(3)
            .any(|l| l.contains("Exit")),
        "sanity: F10 really opened the File popup: {}",
        screen_text(&screen)
    );
    let row = status_row(&screen);
    assert!(!row.contains("F2"), "menu ⇒ universal def: {row}");
    assert!(row.contains("Alt-X"), "…which SHOWS Alt-X: {row}");

    // Esc closes the menu ⇒ the container def returns.
    screen.push_key(Key::Esc, KeyModifiers::default());
    pump(&mut program, 5);
    let row = status_row(&screen);
    assert!(row.contains("F2 Logs"), "back: {row}");
    assert!(!row.contains('^'), "and hidden again: {row}");
}

#[test]
fn ctrl_a_selects_all_filtered_rows_through_the_status_pre_route() {
    let (mut program, screen, core) = start();
    core.borrow_mut().apply(AppMsg::Containers(vec![
        container_state("id1", "web", "running"),
        container_state("id2", "db", "exited"),
        container_state("id3", "cache", "running"),
    ]));
    // `spawn_containers` sends Connection(Up) with its list (message
    // order documented in the Containers spec) — without it the info line
    // keeps showing the checking state instead of the counts.
    core.borrow_mut().apply(AppMsg::Connection(Connection::Up));
    // The windows were built before the seed — REFRESH re-maps them.
    screen.push_event(Event::Broadcast {
        command: tvdocker::bridge::REFRESH,
        source: None,
    });
    pump(&mut program, 5);

    // Tab to the Containers filter and type "running" (2 of 3 rows).
    screen.push_key(Key::Tab, KeyModifiers::default());
    pump(&mut program, 2);
    for k in ['r', 'u', 'n', 'n', 'i', 'n', 'g'] {
        screen.push_key(Key::Char(k), KeyModifiers::default());
        pump(&mut program, 2);
    }
    let text = screen_text(&screen);
    assert!(text.contains("2 / 3 shown"), "filtered view: {text:?}");

    // The chord, through the REAL pump: the status pre-route (which
    // runs before normal dispatch — program.rs) transforms KeyDown into
    // Event::Command(CMD_SELECT_ALL) because the contextual def's key
    // binding is active, and the window's Command arm selects exactly
    // the filtered rows.
    screen.push_key(
        Key::Char('a'),
        KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 5);
    let text = screen_text(&screen);
    assert!(
        text.contains("2 selected"),
        "Ctrl+A via pre-route selected the filtered set: {text:?}"
    );
}

#[test]
fn the_contextual_def_transforms_ctrl_a_into_select_all() {
    // Focus #2's mechanism, minus the pump. The painted "^A" above proves
    // the RIGHT def is selected, not that its key binding is the chord —
    // so drive the real StatusLine the way the pre-route does. With only
    // the universal def, or with `def_one_of` after `def_all`, the chord
    // stays a KeyDown (and reaches the window only through its own arm).
    common::isolate_config(); // this test builds the line itself (no start())
    let mut line = app::init_status_line(Rect::new(0, 0, 80, 1)).expect("status defs");
    line.set_help_ctx(tvdocker::views::containers::HELP_CTX);
    assert_eq!(
        pre_route(&mut line, Event::KeyDown(ctrl_a())),
        Event::Command(app::CMD_SELECT_ALL),
        "a resource window's help ctx must bind the chord to CMD_SELECT_ALL"
    );

    // The same line in a NON-resource context (an open menu's NO_CONTEXT)
    // must leave the chord alone — the universal def binds nothing for it,
    // so the window's own KeyDown arm stays the only path.
    let mut line = app::init_status_line(Rect::new(0, 0, 80, 1)).expect("status defs");
    line.set_help_ctx(tv::HelpCtx::NO_CONTEXT);
    assert_eq!(
        pre_route(&mut line, Event::KeyDown(ctrl_a())),
        Event::KeyDown(ctrl_a()),
        "outside a resource context the universal def binds nothing"
    );
}
