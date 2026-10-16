#![forbid(unsafe_code)]
//! Options ▸ Edit colors… through the real pump: the editor opens modally, a
//! colour edit round-trips through its nested input dialog, committing
//! installs the palette, and naming it writes BOTH the `[themes.<name>]`
//! block and the `theme =` selection — while leaving the rest of the config
//! byte-identical. Cancelling changes nothing.
//!
//! Key staging follows the same two framework gates documented in
//! `theme_menu.rs`: menu items are activated by HOTKEY (Enter would be
//! pre-routed to `CMD_REQ_DETAILS`), `app_commands` is drained only by
//! `run_program`, one harmless event yields run_app's opening pump so the
//! modal opens before its keys arrive, and the commit is `Command::OK` rather
//! than an Enter key.
mod common;

use common::{SharedClock, isolate_config, pump, screen_text};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use tv::event::{Key, KeyModifiers};
use tv::{Command, Event, HeadlessBackend, HeadlessHandle, Program};
use tvdocker::app;
use tvdocker::core::AppCore;

const SEED: &str =
    "# my precious comments\nmouse = true\ntheme = \"dark\"\n\n[polling]\ncontainers_ms = 2000\n";

fn alt() -> KeyModifiers {
    KeyModifiers {
        alt: true,
        ..KeyModifiers::default()
    }
}

fn plain() -> KeyModifiers {
    KeyModifiers::default()
}

/// See `theme_menu.rs`: the config path is process-wide, so the file's tests
/// share one file and must not interleave their seed/assert windows.
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
    pump(&mut program, 5);
    (program, screen, path)
}

fn start() -> (Program, HeadlessHandle, PathBuf) {
    start_with(SEED)
}

/// Stage Options ▸ Edit colors… activation (hotkey `e`) with `pump_once`.
fn stage_editor(program: &mut Program, screen: &HeadlessHandle) {
    screen.push_key(Key::Char('o'), alt());
    pump(program, 4);
    screen.push_key(Key::Char('e'), plain());
    pump(program, 4);
    assert!(
        !screen_text(screen).contains("Settings…"),
        "the Options popup closed ⇒ the item activated"
    );
}

/// One harmless event for run_app's opening pump (see theme_menu.rs).
fn yield_event() -> Event {
    Event::Broadcast {
        command: app::CMD_RELOAD_STATUS,
        source: None,
    }
}

/// Type `s` as individual key presses (the save prompt's input line).
fn typed(s: &str) -> Vec<Event> {
    s.chars()
        .map(|c| Event::KeyDown(tv::event::KeyEvent::new(Key::Char(c), plain())))
        .collect()
}

#[test]
fn editing_a_colour_then_naming_it_writes_the_block_and_the_selection() {
    let _serial = exclusive();
    let (mut program, screen, path) = start();
    stage_editor(&mut program, &screen);

    // Editor: `f` opens tvision's ColorPicker (Presets tab, focused) — `Down`
    // walks the presets and the model follows; the first OK reads that colour
    // into the editor, a second OK commits it. `mytheme` is typed into the
    // save prompt, OK saves.
    screen.push_event(yield_event());
    screen.push_key(Key::Char('f'), plain());
    screen.push_key(Key::Down, plain());
    screen.push_event(Event::Command(Command::OK)); // colour dialog
    screen.push_event(Event::Command(Command::OK)); // editor commits
    for ev in typed("mytheme") {
        screen.push_event(ev);
    }
    screen.push_event(Event::Command(Command::OK)); // save prompt
    screen.push_event(Event::Command(Command::QUIT));
    screen.push_event(Event::Command(Command::QUIT));
    app::run_program(program, None);

    let back = std::fs::read_to_string(&path).expect("config still there");
    assert!(
        back.starts_with("# my precious comments\nmouse = true\n"),
        "the file's head is untouched: {back:?}"
    );
    assert!(
        back.contains("theme = \"mytheme\""),
        "the selection now names the saved theme: {back:?}"
    );
    assert_eq!(
        back.matches("[themes.mytheme.roles]").count(),
        1,
        "one roles table: {back:?}"
    );
    // The cursor role is Background (row 0); its foreground must be what the
    // PICKER left in the model — not the seed's #cdd6f4, which would mean the
    // picker's Down never ran.
    let background = back
        .lines()
        .find(|l| l.starts_with("Background = "))
        .expect("Background serialized");
    assert_ne!(
        background, "Background = \"#cdd6f4/#1e1e2e\"",
        "the picker's Down changed the colour: {back:?}"
    );
    assert!(
        back.contains("[polling]\ncontainers_ms = 2000"),
        "the untouched table survives: {back:?}"
    );
    // …and the file still parses clean against the schema.
    let (cfg, err) = tvdocker::domain::config::TvdockerConfig::load_from(&path);
    assert_eq!(err, None, "the authored file is valid TOML");
    assert_eq!(cfg.theme.as_deref(), Some("mytheme"));
    assert!(
        cfg.validate_themes().is_empty(),
        "the saved theme validates"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn cancelling_the_editor_writes_nothing() {
    let _serial = exclusive();
    let (mut program, screen, path) = start();
    stage_editor(&mut program, &screen);

    screen.push_event(yield_event());
    screen.push_event(Event::Command(Command::CANCEL));
    screen.push_event(Event::Command(Command::QUIT));
    screen.push_event(Event::Command(Command::QUIT));
    app::run_program(program, None);

    let back = std::fs::read_to_string(&path).expect("config still there");
    assert_eq!(
        back, SEED,
        "Cancel must not add a table, move the selection or reformat"
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

/// The Select Color dialog's default OK button, face row.
///
/// Geometry: the editor is 66×21 (tall enough to host the picker inside its
/// own bounds — every modal gate on the stack swallows positional events
/// outside its rect, so a nested dialog may not overhang its parent), hence
/// the 62×21 colour dialog is centered inside it at absolute (9,1).
/// `button_row` puts the row at `h − 3` = 18, with OK spanning local x
/// 38..48 — its `click_rect` accepts local (39..47, 18).
fn select_color_ok() -> tv::Point {
    tv::Point::new(9 + 40, 1 + 18)
}

/// The colour dialog must answer a real mouse click: its OK face is pressed
/// with MouseDown + MouseUp (buttons fire on release, through the
/// MouseTrackCapture the down arms).
///
/// The click is the load-bearing step: if it reaches OK, the colour dialog
/// closes, the NEXT `Command::OK` commits the *editor*, and the name that
/// follows is saved. If the click is lost, that OK closes the *colour*
/// dialog instead, the typed name lands nowhere, and the file stays SEED —
/// which is what this asserts against.
#[test]
fn the_color_dialogs_ok_button_answers_a_mouse_click() {
    let _serial = exclusive();
    let (mut program, screen, path) = start();
    stage_editor(&mut program, &screen);

    screen.push_event(yield_event()); // opens the editor
    screen.push_key(Key::Char('f'), plain()); // opens Select Color
    for ev in click_at(select_color_ok()) {
        screen.push_event(ev);
    }
    screen.push_event(Event::Command(Command::OK)); // editor commits
    for ev in typed("clicked") {
        screen.push_event(ev);
    }
    screen.push_event(Event::Command(Command::OK)); // save prompt
    screen.push_event(Event::Command(Command::QUIT));
    screen.push_event(Event::Command(Command::QUIT));
    app::run_program(program, None);

    let back = std::fs::read_to_string(&path).expect("config still there");
    assert!(
        back.contains("theme = \"clicked\""),
        "the click released OK, the editor committed, and the name saved: {back:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// A CUSTOM theme keeps its own name in the save prompt, so OK re-saves the
/// edits in place — while a preset prompts blank (see `edit_colors`).
#[test]
fn editing_a_custom_theme_prefills_the_save_prompt_with_its_name() {
    let _serial = exclusive();
    let custom = "# my precious comments\nmouse = true\ntheme = \"mine\"\n\n\
                  [themes.mine.roles]\nShadow = \"#010203/#040506\"\n";
    let (mut program, screen, path) = start_with(custom);
    stage_editor(&mut program, &screen);

    // Editor commits unedited; the save prompt opens PREFILLED with "mine",
    // so the immediate OK re-saves it — the one-role seed table is replaced
    // by the full 80-role dump. Had the prompt come up blank, OK would mean
    // "session only" and write nothing at all, which the FrameActive check
    // below would catch.
    screen.push_event(yield_event());
    screen.push_event(Event::Command(Command::OK)); // editor commits
    screen.push_event(Event::Command(Command::OK)); // save prompt keeps "mine"
    screen.push_event(Event::Command(Command::QUIT));
    screen.push_event(Event::Command(Command::QUIT));
    app::run_program(program, None);

    let back = std::fs::read_to_string(&path).expect("config still there");
    assert!(
        back.starts_with("# my precious comments\nmouse = true\n"),
        "the file's head is untouched: {back:?}"
    );
    assert_eq!(
        back.matches("[themes.mine.roles]").count(),
        1,
        "the block was replaced, not duplicated: {back:?}"
    );
    assert!(
        back.contains("Shadow = \"#010203/#040506\""),
        "the seeded role survives resolve → theme_to_def: {back:?}"
    );
    assert!(
        back.contains("FrameActive = \""),
        "the full 80-role dump proves the prefilled name was used: {back:?}"
    );
    let (cfg, err) = tvdocker::domain::config::TvdockerConfig::load_from(&path);
    assert_eq!(err, None, "still valid TOML");
    assert_eq!(cfg.theme.as_deref(), Some("mine"));
    assert!(cfg.validate_themes().is_empty(), "the theme validates");
    let _ = std::fs::remove_file(&path);
}
