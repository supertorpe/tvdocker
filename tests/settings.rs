#![forbid(unsafe_code)]
//! Settings: the Options menu entry, the window, and a
//! save that makes an override take effect through the real pump.
mod common;

use common::{container, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;
use tv::event::{Event, Key, KeyModifiers};
use tv::{Command, HeadlessHandle, Program, View};
use tvdocker::app;
use tvdocker::core::{AppCore, AppMsg, TaskReq};
use tvdocker::domain::config::TvdockerConfig;

/// The suite shares ONE config path per process (`isolate_config`) and runs
/// these tests on threads — serialise everyone who writes it.
static CONFIG_WRITERS: Mutex<()> = Mutex::new(());

fn default_toml() -> String {
    TvdockerConfig::default()
        .to_toml()
        .expect("serialize the default config")
}

/// The default config with `[keybindings]` overrides applied. Building the
/// struct (rather than appending a section) keeps ONE `[keybindings]` table —
/// `default_toml()` already serializes one.
fn config_with(overrides: impl FnOnce(&mut tvdocker::domain::config::Keybindings)) -> String {
    let mut cfg = TvdockerConfig::default();
    overrides(&mut cfg.keybindings);
    cfg.to_toml().expect("serialize the config")
}

/// `isolate_config` only removes a leftover file — the directory is created
/// by `TvdockerConfig::save`, which these tests bypass (they write by hand).
fn write_config(path: &std::path::Path, contents: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("config dir");
    }
    std::fs::write(path, contents).expect("write the config");
}

type TaskRx = tokio::sync::mpsc::UnboundedReceiver<TaskReq>;

fn start() -> (Program, HeadlessHandle, Rc<RefCell<AppCore>>, TaskRx) {
    // One container, so the pre-route has a target when an override says
    // "F8 = Logs" (open_logs needs a cursor row).
    let h = common::app_start(|core| {
        core.borrow_mut()
            .apply(AppMsg::Containers(vec![container("id-a", "webalpha")]));
    });
    (h.program, h.screen, h.core, h.task_rx)
}

/// The Options menu exposes Settings, and it opens the window.
#[test]
fn the_options_menu_opens_the_settings_window() {
    let (mut program, screen, _core, _rx) = start();
    screen.push_key(
        Key::Char('o'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 4);
    let menu = screen_text(&screen);
    assert!(
        menu.contains("Settings"),
        "the Options menu lists Settings: {menu:?}"
    );
    screen.push_key(Key::Char('s'), KeyModifiers::default());
    pump(&mut program, 6);
    let text = screen_text(&screen);
    assert!(
        text.contains("tvdocker.toml"),
        "the window is titled with the config path: {text:?}"
    );
}

/// §9 (Testing): a second invoke FOCUSES the open window instead of opening
/// a second buffer — the ×N openers' pattern. The gate lives in
/// `desktop::open_settings`.
#[test]
fn a_second_settings_invoke_focuses_instead_of_opening_twice() {
    let (mut program, screen, _core, _rx) = start();
    for _ in 0..2 {
        screen.push_event(Event::Command(app::CMD_SETTINGS));
        pump(&mut program, 6);
    }
    let text = screen_text(&screen);
    assert_eq!(
        text.matches("tvdocker.toml").count(),
        1,
        "exactly one Settings window, not two stacked buffers: {text:?}"
    );
}

/// §6/§5: the real save path through the pump — open the window (the editor
/// LOADS the file), press F2, and the status line reports the outcome.
#[test]
fn saving_reports_the_outcome() {
    let path = common::isolate_config();
    let _guard = CONFIG_WRITERS.lock().unwrap_or_else(|e| e.into_inner());
    write_config(&path, &default_toml());
    let (mut program, screen, _core, _rx) = start();
    screen.push_event(Event::Command(app::CMD_SETTINGS));
    pump(&mut program, 8);
    screen.push_key(Key::F(2), KeyModifiers::default());
    pump(&mut program, 8);
    let text = screen_text(&screen);
    assert!(
        text.contains("Settings saved") || text.contains("ignored"),
        "the save reported its outcome: {text:?}"
    );
}

/// §5 live-reload end to end: the window's save broadcasts
/// `CMD_RELOAD_STATUS`, `DynamicStatus` rebuilds, and the next status line
/// emits the new chord — without a restart.
#[test]
fn saving_makes_the_rebuilt_status_line_carry_the_override() {
    let path = common::isolate_config();
    let _guard = CONFIG_WRITERS.lock().unwrap_or_else(|e| e.into_inner());
    // Startup config has NO override: F8 must not reach Logs yet.
    write_config(&path, &default_toml());
    let (mut program, screen, _core, _rx) = start();

    // The config gains an override; the Settings window loads it on open.
    write_config(
        &path,
        &config_with(|kb| kb.open_logs = vec!["F8".to_string()]),
    );
    screen.push_event(Event::Command(app::CMD_SETTINGS));
    pump(&mut program, 8);

    // F2 inside the window: save → report → broadcast → rebuild.
    screen.push_key(Key::F(2), KeyModifiers::default());
    pump(&mut program, 8);
    assert!(
        screen_text(&screen).contains("Settings saved"),
        "the save reported its outcome: {}",
        screen_text(&screen)
    );

    // Back to Containers (the pre-route only uses the FOCUSED def)…
    screen.push_event(Event::Command(Command::CLOSE));
    pump(&mut program, 6);
    assert!(
        !screen_text(&screen).contains("tvdocker.toml"),
        "the Settings window closed: {}",
        screen_text(&screen)
    );

    // …and the rebuilt line emits the new chord.
    screen.push_key(Key::F(8), KeyModifiers::default());
    pump(&mut program, 8);
    assert!(
        screen_text(&screen).contains("Logs — webalpha"),
        "F8 opens Logs after the live reload: {}",
        screen_text(&screen)
    );
}

/// §5: `init_status_line` re-reads the config on every call — that is the
/// assumption `DynamicStatus::rebuild` leans on.
#[test]
fn an_override_reaches_the_status_line_without_a_restart() {
    let path = common::isolate_config();
    let _guard = CONFIG_WRITERS.lock().unwrap_or_else(|e| e.into_inner());
    write_config(
        &path,
        &config_with(|kb| kb.open_logs = vec!["F8".to_string()]),
    );
    let mut line = app::init_status_line(tv::Rect::new(0, 24, 80, 25)).expect("status line");
    // The override lives in the Containers def, which only applies when the
    // line's help ctx is Containers (T2's unit test does the same).
    line.set_help_ctx(tvdocker::views::containers::HELP_CTX);
    let mut out = std::collections::VecDeque::new();
    let mut timers = tv::TimerQueue::new();
    let mut deferred: Vec<tv::Deferred> = Vec::new();
    let mut ctx = tv::Context::new(&mut out, &mut timers, 0, &mut deferred);
    let mut ev = Event::KeyDown(tv::event::KeyEvent::from(Key::F(8)));
    line.handle_event(&mut ev, &mut ctx);
    assert_eq!(
        ev,
        Event::Command(app::CMD_REQ_LOGS),
        "F8 opens Logs when configured"
    );
}
