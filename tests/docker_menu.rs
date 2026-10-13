#![forbid(unsafe_code)]
//! Docker menu open/focus/reopen through the real pump.
mod common;

use common::{SharedClock, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Key, KeyModifiers};
use tv::{HeadlessBackend, HeadlessHandle, Program};
use tvdocker::app;
use tvdocker::core::AppCore;

fn locate(text: &str, needle: &str) -> Option<(usize, usize)> {
    let idx = text.find(needle)?;
    let before = &text[..idx];
    let row = before.matches('\n').count();
    let col = before.rsplit('\n').next().unwrap_or("").chars().count();
    Some((col, row))
}

/// A second `needle` occurrence anywhere AFTER the first one's byte
/// range (row numbers are not byte offsets — box glyphs are multi-byte).
fn second_occurrence(text: &str, needle: &str) -> Option<(usize, usize)> {
    let first = text.find(needle)?;
    locate(&text[first + needle.len()..], needle)
}

fn start() -> (Program, HeadlessHandle, Rc<RefCell<AppCore>>) {
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, _clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    // The wrapper owns a broadcast action sender; these tests never poll, so
    // the receiver may drop (send errs silently — by design). The desktop
    // also wants the app-message channel (Bridge child) — its producer side
    // stays alive here.
    let (action_tx, _action_rx) = tokio::sync::broadcast::channel(1);
    let (app_tx, app_rx) = tokio::sync::mpsc::channel::<tvdocker::core::AppMsg>(8);
    let _keep_alive = app_tx;
    let for_desktop = Rc::clone(&core);
    let (task_tx, _task_rx) = tokio::sync::mpsc::unbounded_channel();
    let program = app::program_with(Box::new(backend), Box::new(shared), move |r| {
        tvdocker::views::desktop::containers_desktop(
            r,
            for_desktop,
            None,
            app_rx,
            action_tx,
            task_tx,
        )
    });
    (program, screen, core)
}

#[test]
fn containers_window_opens_at_startup_and_docker_menu_lists_it() {
    let (mut program, screen, _core) = start();
    // 5, not 3: the grant → COMMAND_SET_CHANGED → status-reprime chain that
    // enables F3's CLOSE transform settles on pump ~4-5 with this window
    // (measured: 3 → stale cache → no transform; 5 → closes).
    for _ in 0..5 {
        program.pump_once();
    }
    let text = screen_text(&screen);
    assert!(
        locate(&text, "Containers").is_some(),
        "startup window : {text:?}"
    );
    // Docker menu opens on Alt-D (its popup adds a second "Containers").
    screen.push_key(
        Key::Char('d'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    for _ in 0..3 {
        program.pump_once();
    }
    let text = screen_text(&screen);
    assert!(
        locate(&text, "Containers").is_some(),
        "window title still on screen"
    );
    assert!(
        second_occurrence(&text, "Containers").is_some(),
        "Docker popup lists Containers: {text:?}"
    );
}

#[test]
fn docker_menu_reopens_a_closed_window() {
    let (mut program, screen, _core) = start();
    // 5, not 3: the grant → COMMAND_SET_CHANGED → status-reprime chain that
    // enables F3's CLOSE transform settles on pump ~4-5 with this window
    // (measured: 3 → stale cache → no transform; 5 → closes).
    for _ in 0..5 {
        program.pump_once();
    }
    screen.push_key(Key::F(3), KeyModifiers::default());
    for _ in 0..4 {
        program.pump_once();
    }
    assert!(
        locate(&screen_text(&screen), "Containers").is_none(),
        "closed: {}",
        screen_text(&screen)
    );
    // Docker → Containers reopens (the Drop flag must have flipped):
    screen.push_key(
        Key::Char('d'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    for _ in 0..3 {
        program.pump_once();
    }
    screen.push_key(Key::Char('c'), KeyModifiers::default()); // hot-letter in Docker menu
    for _ in 0..4 {
        program.pump_once();
    }
    assert!(
        locate(&screen_text(&screen), "Containers").is_some(),
        "reopened after close"
    );
}

#[test]
fn docker_menu_focuses_an_already_open_window_without_duplicating() {
    let (mut program, screen, _core) = start();
    // 5, not 3: the grant → COMMAND_SET_CHANGED → status-reprime chain that
    // enables F3's CLOSE transform settles on pump ~4-5 with this window
    // (measured: 3 → stale cache → no transform; 5 → closes).
    for _ in 0..5 {
        program.pump_once();
    }
    for _ in 0..2 {
        screen.push_key(
            Key::Char('d'),
            KeyModifiers {
                alt: true,
                ..KeyModifiers::default()
            },
        );
        for _ in 0..2 {
            program.pump_once();
        }
        screen.push_key(Key::Char('c'), KeyModifiers::default());
        for _ in 0..3 {
            program.pump_once();
        }
    }
    let text = screen_text(&screen);
    assert!(
        second_occurrence(&text, "Containers").is_none(),
        "exactly one Containers window: {text:?}"
    );
}

#[test]
fn docker_menu_lists_all_six_resource_windows() {
    let (mut program, screen, _core) = start();
    for _ in 0..5 {
        program.pump_once();
    }
    screen.push_key(
        Key::Char('d'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    for _ in 0..3 {
        program.pump_once();
    }
    let text = screen_text(&screen);
    // The popup hangs below the menu bar (rows 1..8). "Containers" also
    // appears as the startup window's TITLE, so it is checked via its
    // second occurrence; the other five are popup-only.
    let popup = text.lines().skip(1).take(7).collect::<Vec<_>>().join("\n");
    for item in ["Images", "Networks", "Volumes", "Events", "Statistics"] {
        assert!(popup.contains(item), "Docker popup lists {item}: {text:?}");
    }
    assert!(
        second_occurrence(&text, "Containers").is_some(),
        "…and Containers: {text:?}"
    );
}

#[test]
fn docker_menu_opens_and_focuses_the_images_window_without_duplicating() {
    let (mut program, screen, _core) = start();
    for _ in 0..5 {
        program.pump_once();
    }
    // Alt-D, then 'i' (Images hot letter,).
    screen.push_key(
        Key::Char('d'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    for _ in 0..3 {
        program.pump_once();
    }
    screen.push_key(Key::Char('i'), KeyModifiers::default());
    for _ in 0..4 {
        program.pump_once();
    }
    assert!(
        locate(&screen_text(&screen), "Images").is_some(),
        "Images window open: {}",
        screen_text(&screen)
    );
    // Invoke AGAIN — open-or-focus must NOT build a second one .
    screen.push_key(
        Key::Char('d'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    for _ in 0..3 {
        program.pump_once();
    }
    screen.push_key(Key::Char('i'), KeyModifiers::default());
    for _ in 0..4 {
        program.pump_once();
    }
    assert!(
        second_occurrence(&screen_text(&screen), "Images").is_none(),
        "exactly one Images window: {}",
        screen_text(&screen)
    );
}
