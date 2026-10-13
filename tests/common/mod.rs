#![forbid(unsafe_code)]
#![allow(dead_code)]
//! Shared helpers for integration tests .
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Once;
use tokio::sync::mpsc::UnboundedReceiver;
use tv::{Clock, HeadlessBackend, HeadlessHandle, ManualClock, Program};
use tvdocker::domain::config::TvdockerConfig;
use tvdocker::domain::docker::containers::ContainerSummary;

/// Retarget this test process's config at a per-process temp file (absent
/// ⇒ virgin) so the suite never reads or writes the developer's real
/// `~/.config/tvdocker/` — otherwise `shell_submit_…` appends entries to
/// it whenever that file already exists. Call from a suite's setup helper;
/// the first call wins, the rest are no-ops.
pub fn isolate_config() -> PathBuf {
    let path = std::env::temp_dir()
        .join(format!("tvdocker-test-config-{}", std::process::id()))
        .join("tvdocker.toml");
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        std::fs::remove_file(&path).ok(); // clear any same-pid leftover
        TvdockerConfig::set_path_override(path.clone());
    });
    path
}

/// Shared handle to an advanceable clock for external tests.
///
/// tvision-rs 0.16 gates `impl Clock for Rc<ManualClock>` behind its own
/// `#[cfg(test)]`, so downstream tests cannot box the `Rc` directly. This
/// wrapper is orphan-rule-safe (our type, their trait) with identical
/// semantics: retain the `Rc` to `advance()`, box the wrapper for the
/// program.
#[derive(Clone)]
pub struct SharedClock(Rc<ManualClock>);

impl SharedClock {
    pub fn new(start_ms: u64) -> (Self, Rc<ManualClock>) {
        let inner = Rc::new(ManualClock::new(start_ms));
        (Self(Rc::clone(&inner)), inner)
    }
}

impl Clock for SharedClock {
    fn now_ms(&self) -> u64 {
        self.0.now_ms()
    }
}

/// The whole screen as one string: one line per row, trailing blanks kept.
pub fn screen_text(screen: &HeadlessHandle) -> String {
    let buf = screen.buffer();
    let mut out = String::new();
    for y in 0..buf.height() {
        for x in 0..buf.width() {
            out.push_str(buf.get(x, y).symbol());
        }
        out.push('\n');
    }
    out
}

/// Pump the event loop `n` times. The startup chain queues several events
/// (grant -> `COMMAND_SET_CHANGED` -> status reprime), so tests measure
/// "settled" in pumps, not in events.
pub fn pump(program: &mut Program, n: usize) {
    for _ in 0..n {
        program.pump_once();
    }
}

/// A running container row with no ports — the seed most harnesses use.
pub fn container(id: &str, name: &str) -> ContainerSummary {
    ContainerSummary {
        id: id.into(),
        name: name.into(),
        image: "img".into(),
        state: "running".into(),
        status: "Up".into(),
        ports: String::new(),
        project: String::new(),
        health: String::new(),
    }
}

/// A container row in an explicit state, with a port (the lifecycle tests).
pub fn container_state(id: &str, name: &str, state: &str) -> ContainerSummary {
    ContainerSummary {
        id: id.into(),
        name: name.into(),
        image: "img".into(),
        state: state.into(),
        status: "Up".into(),
        ports: "80/tcp".into(),
        project: String::new(),
        health: String::new(),
    }
}

/// Everything [`app_start`] hands back; `pub` fields so a test keeps what it
/// asserts on and `_`-patterns the rest.
pub struct AppHarness {
    pub program: Program,
    pub screen: HeadlessHandle,
    pub core: Rc<RefCell<tvdocker::core::AppCore>>,
    pub task_rx: UnboundedReceiver<tvdocker::core::TaskReq>,
    pub clock: Rc<ManualClock>,
}

/// The real `app::build` harness — dynamic status + desktop + bridge + menu.
///
/// The config path is isolated first (otherwise a `[keybindings]` override
/// in the developer's real config rebinds what the tests assert), the
/// startup chain is pumped five times, and `seed` runs BEFORE the build —
/// that is when the desktop maps its rows.
pub fn app_start(seed: impl FnOnce(&Rc<RefCell<tvdocker::core::AppCore>>)) -> AppHarness {
    isolate_config();
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(tvdocker::core::AppCore::new()));
    seed(&core);
    let (task_tx, task_rx) = tokio::sync::mpsc::unbounded_channel();
    let (mut program, _app_tx, _action_tx) = tvdocker::app::build(
        Box::new(backend),
        Box::new(shared),
        Rc::clone(&core),
        None,
        task_tx,
    );
    pump(&mut program, 5);
    AppHarness {
        program,
        screen,
        core,
        task_rx,
        clock,
    }
}
