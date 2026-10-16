#![forbid(unsafe_code)]
//! Chrome integration tests : menu bar, status line, Containers
//! startup window, window-management commands — through the real Program
//! pump.
mod common;

use common::{SharedClock, close_modal_and_quit, dismiss_modal, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key, KeyModifiers};
use tv::{
    Context, Desktop, DrawCtx, HeadlessBackend, HeadlessHandle, ManualClock, MessageBoxButtons,
    MessageBoxKind, Options, Program, Rect, View, ViewState, Window,
};
use tvdocker::app;
use tvdocker::core::AppCore;

type Harness = (Program, HeadlessHandle, Rc<ManualClock>);

fn start() -> Harness {
    // Config isolation, the headless 80x25, a fresh core and five pumps
    // live in `common::app_start` — see there for why isolation matters.
    let h = common::app_start(|_| {});
    (h.program, h.screen, h.clock)
}

/// (column, row) of the first occurrence of `needle` on screen.
/// Columns count **chars**, not bytes — the desktop background is drawn with
/// 3-byte box glyphs, so byte offsets drift.
fn locate(text: &str, needle: &str) -> Option<(usize, usize)> {
    let idx = text.find(needle)?;
    let before = &text[..idx];
    let row = before.matches('\n').count();
    let col = before.rsplit('\n').next().unwrap_or("").chars().count();
    Some((col, row))
}

#[test]
fn startup_renders_menu_bar_status_line_and_containers_title() {
    let (mut program, screen, _clock) = start();
    pump(&mut program, 3);
    let text = screen_text(&screen);
    assert!(
        text.contains("File")
            && text.contains("Docker")
            && text.contains("Windows")
            && text.contains("Help"),
        "menu (order): {text:?}"
    );
    assert!(
        text.contains("F1") && text.contains("F10"),
        "status bar renders (Alt-X is bound but hidden in the containers def): {text:?}"
    );
    // the Containers window opens at startup.
    assert!(
        locate(&text, "Containers").is_some(),
        "startup window: {text:?}"
    );
}

#[test]
fn universal_status_shows_alt_x_when_no_window_is_focused() {
    let (mut program, screen, _clock) = start();
    pump(&mut program, 5);
    // Close the only window (F3) ⇒ focus falls to the desktop ⇒
    // the universal def shows Alt-X again.
    screen.push_key(Key::F(3), KeyModifiers::default());
    pump(&mut program, 4);
    assert!(
        locate(&screen_text(&screen), "Containers").is_none(),
        "window closed"
    );
    assert!(
        screen_text(&screen).contains("Alt-X"),
        "def_all shows Alt-X: {}",
        screen_text(&screen)
    );
}

#[test]
fn f10_then_enter_opens_the_file_menu() {
    let (mut program, screen, _clock) = start();
    // The Containers window's startup chain leaves an event backlog; each
    // pump_once works through it, so key sequences need generous pumps.
    pump(&mut program, 5);
    screen.push_key(Key::F(10), KeyModifiers::default());
    pump(&mut program, 4);
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 4);
    let text = screen_text(&screen);
    // The status line always shows "Alt-X Exit" (row 24); an open File
    // popup shows "Exit" right below the menu bar — locate proves the
    // popup, not just the status text.
    let (_, row) = locate(&text, "Exit").expect("Exit must be on screen");
    assert!(
        row <= 2,
        "File popup opens below the menu bar (row {row}): {text:?}"
    );
}

/// the Help menu offers the update check, ordered
/// Shortcuts | Check for updates… | About. The test only opens the menu —
/// selecting the item would hit the network, and the repository is not
/// configured yet.
#[test]
fn the_help_menu_lists_check_for_updates() {
    let (mut program, screen, _clock) = start();
    pump(&mut program, 5);
    screen.push_key(
        Key::Char('h'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 4);
    let text = screen_text(&screen);
    // Anchor on the FIRST item: it sits immediately under the menu bar, so a
    // row check there proves the popup opened in place regardless of how many
    // items the menu has (the last one drifts down as items are added).
    let (_, first) = locate(&text, "Shortcuts").expect("the Help popup is open");
    assert!(
        first <= 2,
        "the popup opens right under the menu bar (row {first}): {text:?}"
    );
    assert!(
        locate(&text, "Check for updates").is_some(),
        "the item is listed: {text:?}"
    );
    let shortcuts = text.find("Shortcuts").unwrap_or(0);
    let updates = text.find("Check for updates").unwrap_or(0);
    let about = text.find("About").unwrap_or(0);
    assert!(
        shortcuts < updates && updates < about,
        "order is Shortcuts | Check for updates | About: {text:?}"
    );
}

/// …and it must NOT be under File any more (says Help).
#[test]
fn the_file_menu_does_not_offer_check_for_updates() {
    let (mut program, screen, _clock) = start();
    pump(&mut program, 5);
    screen.push_key(Key::F(10), KeyModifiers::default());
    pump(&mut program, 4);
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 4);
    let text = screen_text(&screen);
    let (_, row) = locate(&text, "Exit").expect("the File popup is open");
    assert!(row <= 2, "File popup below the menu bar (row {row})");
    assert!(
        locate(&text, "Check for updates").is_none(),
        "it moved to Help, so File must not offer it: {text:?}"
    );
}

#[test]
fn f1_help_documents_the_table_sort_keys() {
    // Gate A discoverability: ←/→ sorts tables  but the F1 box
    // never said so. Two observable halves:
    //  1. Wiring — CMD_HELP is drained ONLY inside run_app's
    //     app_commands loop (program.rs), so bare pump_once can never open
    //     the box: drive the real entry point (pump primes the status
    //     cache, F1). The open box must swallow Tab+'q': without it, Tab
    //     moves focus from the table (initial) to the filter and 'q'
    //     replaces the "type to filter" placeholder. Enter dismisses, and
    //     TWO QUITs are queued because the modal's blocking pump reads
    //     everything still queued and a QUIT that ends the modal is
    //     discarded by exec's end_state save/restore (framework 0.16).
    //  2. Content — the repaint after dismissal erases the box before
    //     run_program returns, so render the exact constant the
    //     CMD_HELP handler passes to message_box and assert the sort keys.
    let (mut program, screen, _clock) = start();
    pump(&mut program, 5); // status cache priming (see f10 test above)
    screen.push_key(Key::F(1), KeyModifiers::default());
    screen.push_key(Key::Tab, KeyModifiers::default()); // would focus filter
    screen.push_key(Key::Char('q'), KeyModifiers::default()); // would type
    // 0.17: the box is closed by a command it handles, then QUIT ends run_app
    // (see `common::close_modal_and_quit`); Enter no longer dismisses it.
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    let text = screen_text(&screen);
    assert!(
        text.contains("type to filter"),
        "F1 box opened and swallowed Tab+'q' (filter placeholder intact): {text:?}"
    );

    // Content half: the box body exactly as the CMD_HELP handler renders it.
    let (mut program, screen, _clock) = start();
    // 0.17: close the box with a command it handles (see the first half).
    dismiss_modal(&screen);
    let _ = program.message_box(
        app::SHORTCUTS_TEXT,
        MessageBoxKind::Information,
        MessageBoxButtons::ok(),
    );
    let text = screen_text(&screen);
    assert!(
        text.contains("←/→ cycle the sort column") && text.contains("Shift+←/→"),
        "F1 body documents the table sort keys: {text:?}"
    );
}

#[test]
fn f3_closes_the_focused_window() {
    let (mut program, screen, _clock) = start();
    // 5 startup pumps: the grant → COMMAND_SET_CHANGED → status-reprime
    // chain that enables F3's CLOSE transform settles at pump ~4-5 with the
    // Containers window (measured; at 3 the status cache still has CLOSE
    // disabled and the pre-route drops the key as disabled).
    pump(&mut program, 5);
    assert!(
        locate(&screen_text(&screen), "Containers").is_some(),
        "window present before close"
    );
    screen.push_key(Key::F(3), KeyModifiers::default());
    pump(&mut program, 8);
    let text = screen_text(&screen);
    assert!(
        locate(&text, "Containers").is_none(),
        "Containers closed: {text:?}"
    );
}

#[test]
fn f5_zooms_the_containers_window_to_the_desktop_top() {
    let (mut program, screen, _clock) = start();
    pump(&mut program, 5); // see f3 test: the enable chain needs 5 pumps
    let (before_col, before_row) = locate(&screen_text(&screen), "Containers").unwrap();
    screen.push_key(Key::F(5), KeyModifiers::default());
    pump(&mut program, 8);
    let (after_col, after_row) = locate(&screen_text(&screen), "Containers").unwrap();
    // The window starts one row BELOW the desktop top (child coords are
    // desktop-local, desktop origin y=1 → screen row 2); zoom pulls it to
    // the desktop's first screen row. (The centered title's column barely
    // moves — width 76→80 — so the row is the reliable signal.)
    assert_eq!(after_row, 1, "zoomed title sits on the desktop's first row");
    assert!(
        after_row < before_row,
        "zoom moved the title: row {before_row} -> {after_row}"
    );
    let _ = (before_col, after_col);
}

#[test]
fn window_menu_tile_repositions_both_windows() {
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, _clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    let open = Rc::new(RefCell::new(true));
    let mut program = app::program_with(Box::new(backend), Box::new(shared), |r| {
        let mut r = r;
        r.a.y += 1;
        r.b.y -= 1;
        let mut desktop = Desktop::new(r, |br| Some(Desktop::init_background(br)));
        desktop.insert_view(Box::new(tvdocker::views::containers::build(
            Rc::clone(&core),
            Rect::new(2, 1, 78, 23),
            None,
            Rc::clone(&open),
        )));
        let mut second = Window::new(Rect::new(30, 12, 70, 20), Some("second".into()), 2);
        second.state_mut().options.tileable = true;
        desktop.insert_view(Box::new(second));
        Some(Box::new(desktop))
    });
    pump(&mut program, 3);
    screen.push_key(
        Key::Char('w'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    screen.push_key(Key::Char('t'), KeyModifiers::default());
    pump(&mut program, 4);
    let text = screen_text(&screen);
    let (wx, wr) = locate(&text, "Containers").expect("containers visible");
    let (sx, sr) = locate(&text, "second").expect("second visible");
    // Ruling: Desktop::tile uses the framework default orientation (rows
    // first — `tile_columns_first` is private, no setter), so two windows
    // stack vertically: first-inserted on top, second below.
    assert!(
        wr < sr,
        "stacked by tile: containers row {wr} above second row {sr}"
    );
    assert!(sr > wr && sr <= 24, "second moved into its tile slot: {sr}");
    let _ = (wx, sx); // both titles visible — presence asserted above
}

#[test]
fn focused_view_gets_shell_keys_but_status_fkeys_are_pre_routed() {
    // Review I3: the §5 shell key policy through the real pump (no TTY) —
    // a focused shell-like view receives Esc/F2 as KeyDown, while status
    // F-keys are pre-routed to commands and never arrive as KeyDown.
    struct KeyRecorder {
        st: ViewState,
        seen: Rc<RefCell<Vec<Key>>>,
    }
    impl View for KeyRecorder {
        fn state(&self) -> &ViewState {
            &self.st
        }
        fn state_mut(&mut self) -> &mut ViewState {
            &mut self.st
        }
        fn draw(&mut self, _ctx: &mut DrawCtx) {}
        fn handle_event(&mut self, ev: &mut Event, _ctx: &mut Context) {
            if let Event::KeyDown(ke) = ev {
                // Record without clearing — the event must still bubble.
                self.seen.borrow_mut().push(ke.key);
            }
        }
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
    }

    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, _clock) = SharedClock::new(0);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut program = app::program_with(Box::new(backend), Box::new(shared), |r| {
        let mut r = r;
        r.a.y += 1;
        r.b.y -= 1;
        let mut desktop = Desktop::new(r, |br| Some(Desktop::init_background(br)));
        let mut win = Window::new(Rect::new(5, 3, 50, 15), Some("shell".into()), 1);
        win.state_mut().options.tileable = true;
        let mut st = ViewState::new(Rect::new(1, 1, 48, 13));
        st.options = Options {
            selectable: true,
            ..Options::default()
        };
        win.insert_child(Box::new(KeyRecorder {
            st,
            seen: Rc::clone(&seen),
        }));
        desktop.insert_view(Box::new(win));
        Some(Box::new(desktop))
    });
    pump(&mut program, 3);

    // Shell-policy keys arrive as KeyDown…
    screen.push_key(Key::Esc, KeyModifiers::default());
    pump(&mut program, 2);
    screen.push_key(Key::F(2), KeyModifiers::default());
    pump(&mut program, 2);
    // …a status F-key is pre-routed to its command (menu opens) and must
    // never surface as KeyDown to the focused view.
    screen.push_key(Key::F(10), KeyModifiers::default());
    pump(&mut program, 3);

    let seen = seen.borrow();
    assert_eq!(
        *seen,
        vec![Key::Esc, Key::F(2)],
        "Esc/F2 reach the focused shell-like view as KeyDown (§5 policy)"
    );
    assert!(
        !seen.contains(&Key::F(10)),
        "F10 was pre-routed to Command::MENU, never delivered as KeyDown"
    );
}
