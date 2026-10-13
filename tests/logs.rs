#![forbid(unsafe_code)]
//! The Logs window's Esc search rung through the real pump.
mod common;

use common::{SharedClock, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Key, KeyModifiers};
use tv::{Desktop, HeadlessBackend, HeadlessHandle, Program, Rect};
use tvdocker::core::{AppCore, AppMsg};
use tvdocker::views::logs;

fn start() -> (Program, HeadlessHandle, Rc<RefCell<AppCore>>) {
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, _clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    core.borrow_mut().set_logs_session("c1");
    core.borrow_mut().apply(AppMsg::LogLines {
        id: "c1".into(),
        lines: vec![
            tvdocker::domain::docker::logs::LogLine {
                timestamp: String::new(),
                message: "error: boom".into(),
            },
            tvdocker::domain::docker::logs::LogLine {
                timestamp: String::new(),
                message: "all fine".into(),
            },
        ],
    });
    let mut program = Program::new(
        Box::new(backend),
        Box::new(shared),
        tv::Theme::classic_blue(),
        {
            let core = Rc::clone(&core);
            move |r| {
                let mut r = r;
                r.a.y += 1;
                r.b.y -= 1;
                let mut desktop = Desktop::new(r, |br| Some(tv::Desktop::init_background(br)));
                desktop.insert_view(Box::new(logs::build(
                    core,
                    Rect::new(2, 1, 78, 23),
                    "c1",
                    "web",
                    Rc::new(RefCell::new(true)),
                )));
                Some(Box::new(desktop))
            }
        },
        |_| None,
        |_| None,
    );
    pump(&mut program, 3);
    (program, screen, core)
}

#[test]
fn esc_clears_the_search_first_then_closes_the_window() {
    let (mut program, screen, _core) = start();
    let open = || screen_text(&screen).contains("Logs — web");
    assert!(open(), "window at startup");

    screen.push_key(Key::Tab, KeyModifiers::default());
    pump(&mut program, 2);
    for k in ['e', 'r', 'r'] {
        screen.push_key(Key::Char(k), KeyModifiers::default());
        pump(&mut program, 2);
    }
    assert!(screen_text(&screen).contains("search err"), "search active");

    // Rung 1: the search clears, the window stays.
    screen.push_key(Key::Esc, KeyModifiers::default());
    pump(&mut program, 2);
    assert!(open(), "search rung must NOT close the window");
    assert!(
        !screen_text(&screen).contains("search err"),
        "search cleared: {}",
        screen_text(&screen)
    );

    // Rung 2: nothing left — the window closes.
    screen.push_key(Key::Esc, KeyModifiers::default());
    pump(&mut program, 4);
    assert!(!open(), "second Esc closes: {}", screen_text(&screen));
}
