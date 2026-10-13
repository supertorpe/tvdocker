#![forbid(unsafe_code)]
//! Two chords: Ctrl+F5
//! (Size-Move the active window) and Shift+F6 (Previous window).
mod common;

use common::{SharedClock, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Key, KeyModifiers};
use tv::{Desktop, HeadlessBackend, HeadlessHandle, ManualClock, Program, Rect, View, Window};
use tvdocker::app;
use tvdocker::core::AppCore;

fn locate(text: &str, needle: &str) -> Option<(usize, usize)> {
    let idx = text.find(needle)?;
    let before = &text[..idx];
    let row = before.matches('\n').count();
    let col = before.rsplit('\n').next().unwrap_or("").chars().count();
    Some((col, row))
}

/// Status + desktop with `n` plain windows (no Containers needed).
///
/// 5 startup pumps, not 3: every chord test needs the grant →
/// COMMAND_SET_CHANGED → status-reprime chain that enables the
/// RESIZE/PREV/CLOSE transforms (measured in Tasks 6–7).
fn program_with_windows(n: usize) -> (Program, HeadlessHandle, Rc<ManualClock>) {
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    let program = app::program_with_status(
        Box::new(backend),
        Box::new(shared),
        move |r| {
            let mut r = r;
            r.a.y += 1;
            r.b.y -= 1;
            let mut desktop = Desktop::new(r, |br| Some(tv::Desktop::init_background(br)));
            for i in 1..=n {
                let x = 4 + i as i32 * 6;
                let mut win = Window::new(
                    Rect::new(x, i as i32, x + 30, i as i32 + 8),
                    Some(format!("win{i}")),
                    i as i16,
                );
                win.state_mut().options.tileable = true;
                desktop.insert_view(Box::new(win));
            }
            Some(Box::new(desktop))
        },
        {
            let core = Rc::clone(&core);
            move |r| tvdocker::views::status::dynamic_status(r, core)
        },
    );
    (program, screen, clock)
}

#[test]
fn ctrl_f5_enters_the_resize_session_and_arrows_move_the_window() {
    let (mut program, screen, _clock) = program_with_windows(1);
    pump(&mut program, 5);
    let (before_col, before_row) = locate(&screen_text(&screen), "win1").unwrap();
    screen.push_key(
        Key::F(5),
        KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 4);
    screen.push_key(Key::Down, KeyModifiers::default());
    pump(&mut program, 3);
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 4);
    let text = screen_text(&screen);
    let (after_col, after_row) = locate(&text, "win1").unwrap();
    assert!(
        (after_col, after_row) != (before_col, before_row),
        "chord acted: ({before_col},{before_row}) -> ({after_col},{after_row})"
    );
}

#[test]
fn shift_f6_sends_the_current_window_to_the_back() {
    let (mut program, screen, _clock) = program_with_windows(2);
    pump(&mut program, 5);
    // Alt+2 selects window 2 BY NUMBER (program-level handler) — pins
    // "which window is current" regardless of insert/focus order; the F3
    // afterwards then proves the chord moved focus (close acts on current).
    screen.push_key(
        Key::Char('2'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 4);
    screen.push_key(
        Key::F(6),
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 4);
    screen.push_key(Key::F(3), KeyModifiers::default());
    pump(&mut program, 8);
    let text = screen_text(&screen);
    // PREV sent the current (win2) to the back → the current became win1
    // → F3 closed win1 → win2 must still be there, win1 gone.
    assert!(locate(&text, "win2").is_some(), "win2 survives: {text:?}");
    assert!(
        locate(&text, "win1").is_none(),
        "win1 was current and closed: {text:?}"
    );
}

#[test]
fn chords_are_pre_routed_not_delivered_as_keys() {
    // A recording view proves the chords never reach the focused view as
    // KeyDown (status pre-route consumed them into commands).
    use tv::event::Event;
    use tv::{DrawCtx, Rect as TvRect, ViewState};

    struct Rec {
        st: ViewState,
        seen: Rc<RefCell<Vec<Key>>>,
    }
    impl tv::View for Rec {
        fn state(&self) -> &ViewState {
            &self.st
        }
        fn state_mut(&mut self) -> &mut ViewState {
            &mut self.st
        }
        fn draw(&mut self, _ctx: &mut DrawCtx) {}
        fn handle_event(&mut self, ev: &mut Event, _ctx: &mut tv::Context) {
            if let Event::KeyDown(ke) = ev {
                self.seen.borrow_mut().push(ke.key);
            }
        }
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
    }

    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let seen_for_view = Rc::clone(&seen); // the move closure takes this clone
    let mut program = app::program_with_status(
        Box::new(backend),
        Box::new(shared),
        move |r| {
            let mut r = r;
            r.a.y += 1;
            r.b.y -= 1;
            let mut desktop = Desktop::new(r, |br| Some(tv::Desktop::init_background(br)));
            let mut win = Window::new(TvRect::new(4, 2, 50, 14), Some("rec".into()), 1);
            let mut st = ViewState::new(TvRect::new(1, 1, 46, 12));
            st.options = tv::Options {
                selectable: true,
                ..tv::Options::default()
            };
            win.insert_child(Box::new(Rec {
                st,
                seen: Rc::clone(&seen_for_view),
            }));
            desktop.insert_view(Box::new(win));
            Some(Box::new(desktop))
        },
        {
            let core = Rc::clone(&core);
            move |r| tvdocker::views::status::dynamic_status(r, core)
        },
    );
    // 5 pumps: the transform only fires when the status cache has the
    // window's grants; a stale cache would deliver the chord as a raw key
    // and fail this test for the wrong reason (Tasks 6–7 measurement).
    pump(&mut program, 5);
    screen.push_key(
        Key::F(5),
        KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 4);
    screen.push_key(
        Key::F(6),
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 4);
    let seen = seen.borrow();
    assert!(!seen.contains(&Key::F(5)), "Ctrl+F5 pre-routed: {seen:?}");
    assert!(!seen.contains(&Key::F(6)), "Shift+F6 pre-routed: {seen:?}");
    let _ = clock;
}
