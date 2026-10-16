#![forbid(unsafe_code)]
//! Runtime wiring: messages → bridge → core → screen,
//! bounded-channel policy, CLI filter prefill, ActionReq round trip.
mod common;

use common::{SharedClock, close_modal_and_quit, container_state, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key, KeyModifiers};
use tv::{HeadlessBackend, HeadlessHandle, ManualClock, Program};
use tvdocker::app;
use tvdocker::core::{ActionReq, AppCore, AppMsg, Connection};

/// Everything one test iteration hands out: program, screen, clock, shared
/// core, and both channel ends.
type Start = (
    Program,
    HeadlessHandle,
    Rc<ManualClock>,
    Rc<RefCell<AppCore>>,
    tokio::sync::mpsc::Sender<AppMsg>,
    tokio::sync::broadcast::Receiver<ActionReq>,
);

fn start(filter: Option<&str>) -> Start {
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    // The task channel exists but this harness keeps no receiver: these
    // tests assert the AppMsg/ActionReq channels only, and the desktop
    // sends `let _ =` so a dropped receiver is not a panic.
    let (task_tx, _task_rx) = tokio::sync::mpsc::unbounded_channel();
    let (program, app_tx, action_tx) = app::build(
        Box::new(backend),
        Box::new(shared),
        Rc::clone(&core),
        filter,
        task_tx,
    );
    // Subscribe BEFORE returning: the desktop may send PollNow any time.
    let action_rx = action_tx.subscribe();
    (program, screen, clock, core, app_tx, action_rx)
}

#[test]
fn messages_flow_bridge_to_screen_with_status_and_rows() {
    let (mut program, screen, clock, core, app_tx, _action) = start(None);
    // Pump BEFORE advancing: the bridge arms its 50 ms timer on the first
    // startup event (settle broadcast) — arming happens at the then-current
    // clock, so advancing first would push the deadline past the advance.
    pump(&mut program, 1);
    app_tx.try_send(AppMsg::Connection(Connection::Up)).unwrap();
    app_tx
        .try_send(AppMsg::Containers(vec![
            container_state("id1", "web", "running"),
            container_state("id2", "db", "exited"),
        ]))
        .unwrap();
    clock.advance(60);
    // 6, not 4: timers collect only on an IDLE pass, and this window's
    // startup chain (settle → grants → regray broadcasts) runs ~6 pumps.
    pump(&mut program, 6);
    let text = screen_text(&screen);
    assert!(
        text.contains("web") && text.contains("db"),
        "rows on screen: {text:?}"
    );
    assert!(text.contains("connected"), "status right side: {text:?}");
    assert!(text.contains("1/2"), "counts: {text:?}");
    // Only `Containers` bumps the revision (Connection doesn't — Task 3);
    // both messages are proven applied by revision + connection state.
    assert_eq!(core.borrow().revision, 1, "list message applied");
    assert_eq!(
        core.borrow().connection,
        Connection::Up,
        "connection message applied"
    );
}

#[test]
fn startup_with_docker_down_shows_retrying_and_recovers() {
    let (mut program, screen, clock, core, app_tx, _action) = start(None);
    pump(&mut program, 1); // arm the bridge timer at clock=0 (see above)
    app_tx
        .try_send(AppMsg::Connection(Connection::Down {
            error: "no socket".into(),
        }))
        .unwrap();
    clock.advance(60);
    // 6, not 4: timers collect only on an IDLE pass, and this window's
    // startup chain (settle → grants → regray broadcasts) runs ~6 pumps.
    pump(&mut program, 6);
    let text = screen_text(&screen);
    assert!(text.contains("retrying"), "status: {text:?}");
    assert!(
        text.contains("Docker unreachable — retrying…"),
        "info row : {text:?}"
    );

    app_tx.try_send(AppMsg::Connection(Connection::Up)).unwrap();
    app_tx
        .try_send(AppMsg::Containers(vec![container_state(
            "id1", "web", "running",
        )]))
        .unwrap();
    clock.advance(60);
    // 6, not 4: timers collect only on an IDLE pass, and this window's
    // startup chain (settle → grants → regray broadcasts) runs ~6 pumps.
    pump(&mut program, 6);
    let text = screen_text(&screen);
    assert!(
        text.contains("connected") && text.contains("web"),
        "recovered: {text:?}"
    );
    assert_eq!(core.borrow().connection, Connection::Up);
}

#[test]
fn cli_filter_prefills_the_filter_line_and_the_rows() {
    let (mut program, screen, clock, core, app_tx, _act) = start(Some("web"));
    pump(&mut program, 1); // arm the bridge timer at clock=0
    // Rows arrive after startup; only the matching one may show .
    app_tx
        .try_send(AppMsg::Containers(vec![
            container_state("id1", "web", "running"),
            container_state("id2", "db", "exited"),
        ]))
        .unwrap();
    clock.advance(60);
    // 6, not 4: timers collect only on an IDLE pass, and this window's
    // startup chain (settle → grants → regray broadcasts) runs ~6 pumps.
    pump(&mut program, 6);
    let text = screen_text(&screen);
    assert!(text.contains("web"), "matching row visible: {text:?}");
    // Gate C: the filter LINE must show the prefill — the placeholder only
    // renders while the bar's text is empty, so its ABSENCE proves the bar
    // itself was prefilled (contains("web") alone is vacuous: the row name
    // matches too).
    assert!(
        !text.contains("type to filter"),
        "filter line shows the prefill, not the placeholder: {text:?}"
    );
    assert!(
        !text.contains("db"),
        "non-matching row filtered out: {text:?}"
    );
    assert_eq!(
        core.borrow().containers.len(),
        2,
        "core keeps everything; the VIEW filters"
    );
}

#[test]
fn arrow_keys_cycle_the_sort_column_through_the_real_program() {
    // Gate A : tables sort with ←/→ — F5/F6 are Zoom/Next. The
    // widget unit test pinned the cycling, but nothing ever pushed an arrow
    // through the real Program pump, which is how the wrong walkthrough
    // (F5/F6 = sort) shipped.
    let (mut program, screen, clock, _core, app_tx, _act) = start(None);
    pump(&mut program, 1); // arm the bridge timer at clock=0
    app_tx
        .try_send(AppMsg::Containers(vec![
            container_state("id1", "web", "running"),
            container_state("id2", "db", "exited"),
        ]))
        .unwrap();
    clock.advance(60);
    pump(&mut program, 6);
    let text = screen_text(&screen);
    assert!(
        !text.contains("NAME ▲") && !text.contains("NAME ▼"),
        "unsorted at startup: {text:?}"
    );
    // Focus at startup is the table (Task 4 focus flow) — Right → col 0 asc.
    screen.push_key(Key::Right, KeyModifiers::default());
    pump(&mut program, 4);
    let text = screen_text(&screen);
    assert!(text.contains("NAME ▲"), "Right sorts NAME asc: {text:?}");
    let db = text.find("db").expect("db row on screen");
    let web = text.find("web").expect("web row on screen");
    assert!(db < web, "db < web: rows reordered ascending: {text:?}");
    // Left walks backwards → col 4 (PORTS) descending.
    screen.push_key(Key::Left, KeyModifiers::default());
    pump(&mut program, 4);
    let text = screen_text(&screen);
    assert!(text.contains("PORTS ▼"), "Left sorts PORTS desc: {text:?}");
}

#[test]
fn startup_error_shows_a_message_box_before_the_app_runs() {
    // Gate D : run_program(program, Some(err)) must show the
    // config-parse error as a modal BEFORE the event loop — this wiring had
    // no test at all. Observability: the repaint after dismissal erases the
    // box from the headless screen, so its text cannot be asserted once
    // run_program returns (the live text is Gate 4's manual walk + the PTY
    // capture). What IS observable: the open modal SWALLOWS Tab+'q' —
    // without the box, Tab would move focus from the table (initial) to the
    // filter and 'q' would replace the "type to filter" placeholder.
    // The box is up before the loop (run_program_with shows it directly), so
    // its blocking pump reads Tab+'q' (swallowed) and then the close: on 0.17
    // Enter does NOT dismiss a box (a non-default button ignores
    // Command::DEFAULT and its activation timer never fires on a still clock)
    // and a QUIT delivered while the box is up is dropped — so close it with a
    // command it handles, then QUIT ends run_app (`common::close_modal_and_quit`).
    let (program, screen, _clock, _core, _app, _act) = start(None);
    screen.push_key(Key::Tab, KeyModifiers::default()); // would focus filter
    screen.push_key(Key::Char('q'), KeyModifiers::default()); // would filter
    close_modal_and_quit(&screen);
    app::run_program(program, Some("Config parse error: bad.toml".into()));
    let text = screen_text(&screen);
    assert!(
        text.contains("type to filter"),
        "the startup box swallowed the queued 'q' (filter placeholder intact): {text:?}"
    );
}

#[test]
fn appmsg_channel_is_bounded_and_drops_new_when_full() {
    let (_program, _screen, _clock, _core, app_tx, _act) = start(None);
    let mut kept = 0usize;
    for _ in 0..(app::APP_CHANNEL_CAP * 5) {
        if app_tx.try_send(AppMsg::Connection(Connection::Up)).is_ok() {
            kept += 1;
        }
    }
    assert_eq!(
        kept,
        app::APP_CHANNEL_CAP,
        "drop-new policy: exactly capacity messages kept"
    );
}

#[test]
fn menu_command_enqueues_a_poll_now_action() {
    let (mut program, screen, _clock, _core, _app, mut action_rx) = start(None);
    pump(&mut program, 3);
    // Alt+D → Docker menu, 'c' → Containers (which also asks for a poll).
    screen.push_key(
        Key::Char('d'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    screen.push_key(Key::Char('c'), KeyModifiers::default());
    pump(&mut program, 4);
    assert_eq!(
        action_rx.try_recv().unwrap(),
        ActionReq::PollNow,
        "opening/focusing Containers requests an immediate poll (loop)"
    );
}

#[test]
fn opening_another_resource_window_polls_again() {
    let (mut program, screen, _clock, _core, _app, mut action_rx) = start(None);
    pump(&mut program, 5);
    // Containers (Alt-D, 'c'):
    screen.push_key(
        Key::Char('d'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    screen.push_key(Key::Char('c'), KeyModifiers::default());
    pump(&mut program, 4);
    assert_eq!(action_rx.try_recv().unwrap(), ActionReq::PollNow);

    // Images (Alt-D, 'i'): a SECOND PollNow for the SAME subscriber —
    // the broadcast wakes every poller on every open/focus.
    screen.push_key(
        Key::Char('d'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    screen.push_key(Key::Char('i'), KeyModifiers::default());
    pump(&mut program, 4);
    assert_eq!(
        action_rx.try_recv().unwrap(),
        ActionReq::PollNow,
        "each Docker-menu open/focus re-requests a poll of ALL lists"
    );
}

#[test]
fn bridge_accepts_appcore_without_interface_changes() {
    // The Bridge's interface pinned: same Bridge, new state type.
    use tv::{Desktop, Theme};
    use tvdocker::bridge::{Bridge, REFRESH};
    let (backend, screen) = HeadlessBackend::new(40, 8);
    let (shared_clock, clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    let (tx, rx) = tokio::sync::mpsc::channel::<AppMsg>(8);
    let mut program = Program::new(
        Box::new(backend),
        Box::new(shared_clock),
        Theme::classic_blue(),
        {
            let core = Rc::clone(&core);
            move |r| {
                let mut r = r;
                r.a.y += 1;
                r.b.y -= 1;
                let mut desktop = Desktop::new(r, |br| Some(tv::Desktop::init_background(br)));
                desktop.insert_view(Box::new(Bridge::new(core, rx, AppCore::apply)));
                Some(Box::new(desktop))
            }
        },
        |_| None,
        |_| None,
    );
    screen.push_event(Event::Broadcast {
        command: REFRESH,
        source: None,
    });
    pump(&mut program, 2);
    tx.try_send(AppMsg::Containers(vec![container_state(
        "x", "y", "running",
    )]))
    .unwrap();
    clock.advance(60);
    // 6, not 4: timers collect only on an IDLE pass, and this window's
    // startup chain (settle → grants → regray broadcasts) runs ~6 pumps.
    pump(&mut program, 6);
    assert_eq!(
        core.borrow().containers.len(),
        1,
        "Bridge<AppCore, AppMsg> works unchanged"
    );
}

#[test]
fn tab_order_is_filter_then_table() {
    // Tab cycles filter ↔ table (the bar is non-selectable).
    // Focus evidence (Task 4 probe): after settle the TABLE is current;
    // one Tab lands on the FILTER.
    let (mut program, screen, _clock, _core, app_tx, _act) = start(None);
    app_tx
        .try_send(AppMsg::Containers(vec![container_state(
            "id1", "web", "running",
        )]))
        .unwrap();
    pump(&mut program, 3);
    screen.push_key(Key::Tab, KeyModifiers::default()); // table → filter
    pump(&mut program, 2);
    screen.push_key(Key::Char('q'), KeyModifiers::default());
    pump(&mut program, 3);
    assert!(screen_text(&screen).contains('q'), "filter receives typing");
    // Tab → table: typing now goes nowhere the filter can see.
    screen.push_key(Key::Tab, KeyModifiers::default());
    pump(&mut program, 2);
    screen.push_key(Key::Char('y'), KeyModifiers::default());
    pump(&mut program, 2);
    assert!(!screen_text(&screen).contains('y'), "focus left the filter");
    // Tab again (bar skipped) → back on the filter.
    screen.push_key(Key::Tab, KeyModifiers::default());
    pump(&mut program, 2);
    screen.push_key(Key::Char('z'), KeyModifiers::default());
    // 4, not 2: with the startup deferred/broadcast backlog still queued,
    // a fresh keystroke surfaces on a later pass (measured: caret/focus was
    // already on the filter; the char simply hadn't been dequeued yet).
    pump(&mut program, 4);
    assert!(
        screen_text(&screen).contains("qz"),
        "bar skipped; cycle filter ↔ table"
    );
}

#[test]
fn images_messages_flow_into_their_window_through_the_bridge() {
    use tvdocker::domain::docker::images::ImageEntry;
    let (mut program, screen, clock, _core, app_tx, _action) = start(None);
    pump(&mut program, 3);
    // Open Images from the Docker menu (hot letter 'i').
    screen.push_key(
        Key::Char('d'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    screen.push_key(Key::Char('i'), KeyModifiers::default());
    pump(&mut program, 4);
    // A poll result arrives on the REAL bounded channel:
    app_tx
        .try_send(AppMsg::Images(vec![ImageEntry {
            id: "sha256:aaaa11112222".into(),
            repository: "nginx".into(),
            tag: "1.27".into(),
            size: 1_500_000,
        }]))
        .expect("channel open");
    clock.advance(60); // the bridge timer drains on its next tick
    pump(&mut program, 6); // timers collect on an idle pass
    let text = screen_text(&screen);
    assert!(text.contains("nginx"), "rows on screen: {text:?}");
    assert!(text.contains("1.5MB"), "formatted SIZE column: {text:?}");
}
