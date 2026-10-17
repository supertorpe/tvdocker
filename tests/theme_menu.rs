#![forbid(unsafe_code)]
//! Options ▸ Themes… through the real pump: the menu command must reach
//! `run_app` (no view consumes it), the picker must open modally, confirming
//! with OK must install the palette AND persist the new `theme = "…"` while
//! leaving the rest of the config byte-identical, and Esc must change
//! nothing.
//!
//! Two framework gates shape the key staging (see `resource_actions.rs` for
//! the first):
//!
//! 1. **`exec_view_with` runs under `run_app`'s private `pump_and_drive`** —
//!    `pump_once` leaves `Program::pending_modal` pending forever. So the
//!    picker's keys are queued only after staging, and the drive ends with a
//!    command the picker handles (`OK`) followed by a `QUIT` that ends
//!    `run_app` (`0.17`: a bare `QUIT` is *dropped* while a box is up).
//! 2. **`app_commands` is drained ONLY by `run_app`** (`program.rs:919`), so
//!    the menu activation may be staged with plain `pump_once` calls and the
//!    command simply waits there. The modal then opens at the END of
//!    run_app's first iteration — which is why that iteration is fed one
//!    harmless event first: it would otherwise eat the picker's first key.
//!
//! Items are activated by their **hotkey, not Enter**: the status line
//! pre-routes `Enter` to `CMD_REQ_DETAILS` while the Containers help context
//! is active — *before* captures run — so Enter closes the menu with the
//! Details command instead of selecting (that is why no existing test picks a
//! menu item with Enter). `t` is `Themes…`'s mnemonic and is unbound
//! elsewhere.
mod common;

use common::{SharedClock, isolate_config, pump, screen_text};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use tv::event::{Key, KeyModifiers};
use tv::{Command, Event, HeadlessBackend, HeadlessHandle, Program};
use tvdocker::app;
use tvdocker::core::AppCore;

/// A config with a comment, two root keys, a selection, a blank line and a
/// table — the surrounding text the surgical writer must reproduce. It must
/// also PARSE: the seed is what the app loads at startup, so a schema-invalid
/// file would silently fall back to defaults and the palette assertions would
/// see `classic`.
const SEED: &str =
    "# my precious comments\nmouse = true\ntheme = \"dark\"\n\n[polling]\ncontainers_ms = 2000\n";
/// The same file with the selection replaced in place.
const SELECTED: &str =
    "# my precious comments\nmouse = true\ntheme = \"light\"\n\n[polling]\ncontainers_ms = 2000\n";

fn alt() -> KeyModifiers {
    KeyModifiers {
        alt: true,
        ..KeyModifiers::default()
    }
}

fn plain() -> KeyModifiers {
    KeyModifiers::default()
}

/// `set_path_override` is process-wide (first call wins), so BOTH tests here
/// share one config file. They seed it at start and assert exact bytes at the
/// end — running them in parallel would let one test's seed land on top of the
/// other's committed selection (measured: exactly that flake). A mutex makes
/// the file's tests sequential; poisoning is ignored so one failure does not
/// cascade into the next.
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Hermetic build over `seed` so the assertions can be about exact bytes.
fn start_with(seed: &str) -> (Program, HeadlessHandle, PathBuf) {
    let path = isolate_config();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("config dir");
    }
    std::fs::write(&path, seed).expect("seed the isolated config");
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, _clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    let (action_tx, _action_rx) = tokio::sync::broadcast::channel(1);
    let (app_tx, app_rx) = tokio::sync::mpsc::channel::<tvdocker::core::AppMsg>(8);
    let _keep_alive = app_tx;
    let for_desktop = Rc::clone(&core);
    let (task_tx, _task_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut program = app::program_with(Box::new(backend), Box::new(shared), move |r| {
        tvdocker::views::desktop::containers_desktop(
            r,
            for_desktop,
            None,
            app_rx,
            action_tx,
            task_tx,
        )
    });
    pump(&mut program, 5); // startup + status priming (chords precedent)
    (program, screen, path)
}

fn start() -> (Program, HeadlessHandle, PathBuf) {
    start_with(SEED)
}

/// Stage the Options ▸ Themes… activation with `pump_once`: the popup needs a
/// few passes, and the hotkey posts `CMD_THEMES` into `app_commands`, which
/// waits there until `run_program` drains it.
fn stage_theme_menu(program: &mut Program, screen: &HeadlessHandle) {
    screen.push_key(Key::Char('o'), alt());
    pump(program, 4);
    screen.push_key(Key::Char('t'), plain()); // mnemonic of "Themes…"
    pump(program, 4);
    // The menu must be gone (the item fired) before the keys below are queued.
    assert!(
        !screen_text(screen).contains("Settings…"),
        "the Options popup closed ⇒ the item activated"
    );
}

/// One harmless event for run_app's opening pump — without it, that pump
/// eats the picker's first key before `CMD_THEMES` is drained and the modal
/// opens.
fn yield_event() -> Event {
    Event::Broadcast {
        command: app::CMD_RELOAD_STATUS,
        source: None,
    }
}

fn locate(text: &str, needle: &str) -> Option<(usize, usize)> {
    let idx = text.find(needle)?;
    let before = &text[..idx];
    let row = before.matches('\n').count();
    let col = before.rsplit('\n').next().unwrap_or("").chars().count();
    Some((col, row))
}

#[test]
fn themes_menu_applies_the_palette_and_rewrites_only_the_selection_line() {
    let _serial = exclusive();
    let (mut program, screen, path) = start();
    stage_theme_menu(&mut program, &screen);

    // Inside the picker: `l` type-selects `light` (the seed selected `dark`).
    // The commit is `Command::OK`, not the Enter KEY — the documented
    // `resource_actions.rs` idiom: a pre-queued stream never idles between
    // events, so the status line still holds the Containers def and a raw
    // Enter would be pre-routed to `CMD_REQ_DETAILS` instead of reaching the
    // modal (a live human's Enter works because the idle pass re-targets the
    // status line at the modal's NO_CONTEXT ctx).
    screen.push_event(yield_event());
    screen.push_key(Key::Char('l'), plain());
    screen.push_event(Event::Command(Command::OK));
    screen.push_event(Event::Command(Command::QUIT));
    screen.push_event(Event::Command(Command::QUIT));
    app::run_program(program, None);

    // 1. Persistence: the selection replaced in place, everything else
    //    byte-identical.
    let back = std::fs::read_to_string(&path).expect("config still there");
    assert_eq!(
        back, SELECTED,
        "only the `theme` value changed; comments/formatting preserved"
    );

    // 2. Application: the surviving chrome is painted in the NEW palette —
    //    the Containers title uses FrameActive, whose background is the
    //    theme's `background` slot (light's #ececec, not dark's #1e1e2e).
    let text = screen_text(&screen);
    let (col, row) = locate(&text, "Containers").expect("window still up");
    let buf = screen.buffer();
    let bg = buf.get(col as u16, row as u16).style().bg;
    assert_eq!(
        bg,
        tv::Color::Rgb(0xec, 0xec, 0xec),
        "set_theme took effect on the surviving chrome: {text:?}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn escape_leaves_the_config_file_and_palette_untouched() {
    let _serial = exclusive();
    let (mut program, screen, path) = start();
    stage_theme_menu(&mut program, &screen);

    screen.push_event(yield_event());
    screen.push_key(Key::Esc, plain()); // cancel: never applies, never writes
    screen.push_event(Event::Command(Command::QUIT));
    screen.push_event(Event::Command(Command::QUIT));
    app::run_program(program, None);

    let back = std::fs::read_to_string(&path).expect("config still there");
    assert_eq!(back, SEED, "Esc must not add, reorder or reformat anything");
    let text = screen_text(&screen);
    let (col, row) = locate(&text, "Containers").expect("the app survived");
    let buf = screen.buffer();
    let bg = buf.get(col as u16, row as u16).style().bg;
    assert_eq!(
        bg,
        tv::Color::Rgb(0x1e, 0x1e, 0x2e),
        "Esc keeps the seeded `dark` palette: {text:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// A left click press/release at an ABSOLUTE screen position.
fn click_at(p: tv::Point) -> [Event; 2] {
    let press = |left: bool| {
        Event::MouseDown(tv::MouseEvent {
            position: p,
            buttons: tv::MouseButtons {
                left,
                ..tv::MouseButtons::default()
            },
            ..tv::MouseEvent::default()
        })
    };
    let mut up = match press(true) {
        Event::MouseDown(m) => Event::MouseUp(m),
        _ => unreachable!(),
    };
    if let Event::MouseUp(m) = &mut up {
        m.buttons.left = false;
    }
    [press(true), up]
}

/// The Themes dialog's default OK button, face row.
///
/// Geometry: `picker_rect` centers the 70×17 dialog in the desktop-local
/// extent (80×23) → absolute (5,3); `button_row` puts the row at `h − 3` =
/// 14 with OK at local x 46..56, so its `click_rect` accepts local (47..55,
/// 14) ⇒ absolute (53,17).
fn themes_ok() -> tv::Point {
    tv::Point::new(53, 17)
}

/// The dialog's OK button must answer a real mouse click: pressing it
/// commits the highlighted entry, and the picker only rewrites the SEED's
/// selection line — so the test moves the cursor first and asserts the
/// selection actually CHANGES (a lost click leaves `dark` behind).
#[test]
fn the_themes_dialogs_ok_button_answers_a_mouse_click() {
    let _serial = exclusive();
    let (mut program, screen, path) = start();
    stage_theme_menu(&mut program, &screen);

    screen.push_event(yield_event());
    screen.push_key(Key::Down, plain()); // cursor: dark → light
    for ev in click_at(themes_ok()) {
        screen.push_event(ev);
    }
    screen.push_event(Event::Command(Command::QUIT));
    screen.push_event(Event::Command(Command::QUIT));
    app::run_program(program, None);

    let back = std::fs::read_to_string(&path).expect("config still there");
    assert!(
        back.contains("theme = \"light\""),
        "the click committed the highlighted entry: {back:?}"
    );
    let _ = std::fs::remove_file(&path);
}
