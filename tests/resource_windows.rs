#![forbid(unsafe_code)]
//! Two resource windows open at once keep independent state
//! ("independent, closeable, re-openable").
mod common;

use common::{SharedClock, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Key, KeyModifiers};
use tv::{Desktop, HeadlessBackend, HeadlessHandle, ManualClock, Program, Rect};
use tvdocker::app;
use tvdocker::core::{AppCore, AppMsg, Connection};
use tvdocker::domain::docker::images::ImageEntry;
use tvdocker::domain::docker::volumes::VolumeEntry;
use tvdocker::views::{images, volumes};

fn image(name: &str) -> ImageEntry {
    ImageEntry {
        id: format!("id-{name}"),
        repository: name.into(),
        tag: "latest".into(),
        size: 1_000,
    }
}

fn volume(name: &str) -> VolumeEntry {
    VolumeEntry {
        name: name.into(),
        driver: "local".into(),
        mountpoint: format!("/mnt/{name}"),
        size: 0,
    }
}

/// Status + desktop with Images and Volumes stacked (non-overlapping
/// rects, so both info lines are assertable in full — each needs ~40
/// columns and 80×25 has room for only one at a time). The status line
/// is the dynamic one: F6 (Next) must pre-route through it to switch
/// windows.
fn start() -> (
    Program,
    HeadlessHandle,
    Rc<ManualClock>,
    Rc<RefCell<AppCore>>,
) {
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    let program = app::program_with_status(
        Box::new(backend),
        Box::new(shared),
        {
            let core = Rc::clone(&core);
            move |r| {
                let mut r = r;
                r.a.y += 1;
                r.b.y -= 1;
                let mut desktop = Desktop::new(r, |br| Some(tv::Desktop::init_background(br)));
                let open_a = Rc::new(RefCell::new(false));
                let open_b = Rc::new(RefCell::new(false));
                desktop.insert_view(Box::new(images::build(
                    Rc::clone(&core),
                    Rect::new(2, 1, 79, 12),
                    Rc::clone(&open_a),
                )));
                desktop.insert_view(Box::new(volumes::build(
                    Rc::clone(&core),
                    Rect::new(2, 12, 79, 23),
                    Rc::clone(&open_b),
                )));
                Some(Box::new(desktop))
            }
        },
        {
            let core = Rc::clone(&core);
            move |r| tvdocker::views::status::dynamic_status(r, core)
        },
    );
    (program, screen, clock, core)
}

#[test]
fn two_resource_windows_keep_independent_filters() {
    let (mut program, screen, _clock, core) = start();
    core.borrow_mut().apply(AppMsg::Connection(Connection::Up));
    core.borrow_mut()
        .apply(AppMsg::Images(vec![image("nginx"), image("redis")]));
    core.borrow_mut()
        .apply(AppMsg::Volumes(vec![volume("backups"), volume("cache")]));
    // The windows were built BEFORE the seed — REFRESH re-maps them.
    screen.push_event(tv::event::Event::Broadcast {
        command: tvdocker::bridge::REFRESH,
        source: None,
    });
    pump(&mut program, 5); // startup + status priming (chords precedent)

    let text = screen_text(&screen);
    assert_eq!(
        text.matches("2 / 2 shown").count(),
        2,
        "both windows list both of their own rows: {text:?}"
    );
    assert!(
        text.contains("nginx") && text.contains("backups"),
        "rows drawn: {text:?}"
    );

    // VOLUMES is focused (inserted last). Tab to its filter, type —
    // only its counts may change.
    screen.push_key(Key::Tab, KeyModifiers::default());
    pump(&mut program, 2);
    for k in ['c', 'a', 'c', 'h'] {
        screen.push_key(Key::Char(k), KeyModifiers::default());
        pump(&mut program, 2);
    }
    let text = screen_text(&screen);
    assert!(text.contains("filter cach"), "volumes filtered: {text:?}");
    assert_eq!(
        text.matches("2 / 2 shown").count(),
        1,
        "the IMAGES window keeps its rows and its empty filter: {text:?}"
    );
    assert!(
        text.contains("filter -"),
        "images filter untouched: {text:?}"
    );

    // F6 (Next, pre-routed by the status line) → the Images window;
    // give IT a different filter.
    screen.push_key(Key::F(6), KeyModifiers::default());
    pump(&mut program, 3);
    screen.push_key(Key::Tab, KeyModifiers::default());
    pump(&mut program, 2);
    for k in ['n', 'g', 'i', 'n'] {
        screen.push_key(Key::Char(k), KeyModifiers::default());
        pump(&mut program, 2);
    }
    let text = screen_text(&screen);
    assert!(text.contains("filter ngin"), "images filtered: {text:?}");
    assert!(
        text.contains("filter cach"),
        "volumes KEPT its filter while the other window typed: {text:?}"
    );
    assert_eq!(
        text.matches("1 / 2 shown").count(),
        2,
        "both windows at 1/2 — each by its own needle: {text:?}"
    );
}
