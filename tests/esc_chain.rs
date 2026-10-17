#![forbid(unsafe_code)]
//! The Esc priority chain : exit multi-select → clear filter →
//! close the active window. Program-based because rung 3 needs the real
//! close pipeline (the framework itself has no Esc→close path).
mod common;

use common::{SharedClock, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Key, KeyModifiers};
use tv::{Desktop, HeadlessBackend, Program, Rect};
use tvdocker::core::AppCore;
use tvdocker::views::containers;

fn core_with_rows() -> Rc<RefCell<AppCore>> {
    let core = Rc::new(RefCell::new(AppCore::new()));
    let row = |id: &str, name: &str| tvdocker::domain::docker::containers::ContainerSummary {
        id: id.into(),
        name: name.into(),
        image: "img".into(),
        state: "running".into(),
        status: "Up".into(),
        ports: "80/tcp".into(),
        project: String::new(),
        health: String::new(),
    };
    // Two rows: the upstream multi-select check cell shows `[X]` on a marked
    // row (`[ ]` otherwise), so rung 1 needs a second row to move the cursor
    // off the marked one.
    core.borrow_mut()
        .apply(tvdocker::core::AppMsg::Containers(vec![
            row("id1", "web"),
            row("id2", "db"),
        ]));
    core
}

#[test]
fn esc_clears_selection_first_then_filter_then_closes() {
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, _clock) = SharedClock::new(0);
    let core = core_with_rows();
    let mut program: Program = Program::new(
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
                desktop.insert_view(Box::new(containers::build(
                    core,
                    Rect::new(2, 1, 78, 23),
                    None,
                    Rc::new(RefCell::new(true)),
                )));
                Some(Box::new(desktop))
            }
        },
        |_| None,
        |_| None,
    );
    pump(&mut program, 5);
    let title_open = || screen_text(&screen).contains("Containers");

    // Focus evidence (probe-determined): after settle the TABLE is current
    // (reset_current walks from the top-most selectable child down — the
    // table is inserted last), and one Tab moves focus to the FILTER.
    //
    // Rung 1: Esc exits multi-select — Space on the focused table, then
    // Down so the selected row shows '*' instead of the cursor's '>'.
    screen.push_key(Key::Char(' '), KeyModifiers::default());
    pump(&mut program, 3);
    screen.push_key(Key::Down, KeyModifiers::default());
    pump(&mut program, 2);
    assert!(
        screen_text(&screen).contains("[X]"),
        "row selected before Esc"
    );
    screen.push_key(Key::Esc, KeyModifiers::default());
    pump(&mut program, 2);
    assert!(title_open(), "rung 1 must NOT close the window");
    assert!(
        !screen_text(&screen).contains("[X]"),
        "rung 1 cleared the selection"
    );

    // Rung 2: Esc clears the filter text. Tab moves table → filter (3 pumps,
    // not 2: the nested Table's extra focus broadcasts settle a pass later).
    screen.push_key(Key::Tab, KeyModifiers::default());
    pump(&mut program, 3);
    screen.push_key(Key::Char('q'), KeyModifiers::default());
    pump(&mut program, 3);
    assert!(screen_text(&screen).contains('q'), "filter holds the text");
    screen.push_key(Key::Esc, KeyModifiers::default());
    pump(&mut program, 2);
    assert!(title_open(), "rung 2 must NOT close the window");
    assert!(
        !screen_text(&screen).contains('q'),
        "rung 2 cleared the filter"
    );

    // Rung 3: nothing left — the window closes.
    screen.push_key(Key::Esc, KeyModifiers::default());
    pump(&mut program, 4);
    assert!(!title_open(), "rung 3 closes the active window");
}
