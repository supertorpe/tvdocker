#![forbid(unsafe_code)]
//! Help → Check for updates… with a self-updatable release: the check
//! asks Yes/No before anything is downloaded; Yes hands
//! `TaskReq::UpdateInstall` to the worker, No installs nothing. The
//! download itself never runs here — it lives in the worker arm
//! (pinned hermetically in `domain::docker::tasks`).
//!
//! **Modal tests drive `app::run_program_with`, not bare `pump_once`**
//! (tests/resource_actions.rs documents why): pre-queue every key, answer the
//! box, then `close_modal_and_quit` (0.17: a QUIT is dropped while a box is up,
//! and the check may show a second "result" box after the answer).
mod common;

use common::{SharedClock, close_modal_and_quit, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::Command;
use tv::event::{Event, Key, KeyModifiers};
use tv::{HeadlessBackend, HeadlessHandle, Program};
use tvdocker::app;
use tvdocker::core::{AppCore, TaskReq};
use tvdocker::domain::update::{Outcome, Release};

type TaskTx = tokio::sync::mpsc::UnboundedSender<TaskReq>;
type TaskRx = tokio::sync::mpsc::UnboundedReceiver<TaskReq>;

/// A newer release whose asset matches this target (self-updatable) or
/// one that only points at the release page.
fn release(asset: Option<&str>) -> Release {
    Release {
        version: "9.9.9".into(),
        download_url: asset
            .map(str::to_string)
            .unwrap_or("https://github.com/supertorpe/tvdocker/releases/latest".into()),
        asset: asset.map(str::to_string),
    }
}

fn start() -> (
    Program,
    HeadlessHandle,
    Rc<RefCell<AppCore>>,
    TaskTx,
    TaskRx,
) {
    common::isolate_config();
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, _clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    let (task_tx, task_rx) = tokio::sync::mpsc::unbounded_channel();
    // The desktop owns one handle (real wiring); the run_program
    // command handler gets the other — the test keeps the receiver.
    let (mut program, _app_tx, _action_tx) = app::build(
        Box::new(backend),
        Box::new(shared),
        Rc::clone(&core),
        None,
        task_tx.clone(),
    );
    pump(&mut program, 5); // startup + status priming (chords precedent)
    (program, screen, core, task_tx, task_rx)
}

/// Open Help → Check for updates… (Alt-H, then the `C` hot letter),
/// queue `answer` for the Yes/No box plus two QUITs, then drive the
/// real entry point with an injected check (no network).
fn drive_check(
    program: Program,
    screen: &HeadlessHandle,
    task_tx: &TaskTx,
    answer: Command,
    outcome: Outcome,
) {
    screen.push_key(
        Key::Char('h'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
    );
    screen.push_key(Key::Char('c'), KeyModifiers::default());
    screen.push_event(Event::Command(answer));
    // 0.17: after the answer the check may show a SECOND box (the result
    // "describe" modal on No / no-asset), and a QUIT is dropped while any box
    // is up — so close a leftover box, then QUIT ends run_app.
    close_modal_and_quit(screen);
    let check = move |_current: &str| Ok(outcome.clone());
    app::run_program_with(program, None, task_tx, "0.1.0", check);
}

#[test]
fn confirming_the_offer_hands_the_install_to_the_worker() {
    let (program, screen, core, task_tx, mut task_rx) = start();
    // The Yes/No box is really up: stray keys never reach the filter.
    screen.push_key(Key::Char('x'), KeyModifiers::default());
    screen.push_key(Key::Char('x'), KeyModifiers::default());
    drive_check(
        program,
        &screen,
        &task_tx,
        Command::YES,
        Outcome::Available(release(Some("https://example/tvdocker_9.9.9.tar.gz"))),
    );
    let req = task_rx.try_recv().expect("the install request was posted");
    let TaskReq::UpdateInstall { version, url } = req else {
        panic!("the worker was asked to install the offered release");
    };
    assert_eq!(version, "9.9.9", "the offered version");
    assert_eq!(
        url, "https://example/tvdocker_9.9.9.tar.gz",
        "exactly the asset URL the check reported"
    );
    let text = screen_text(&screen);
    assert!(
        text.contains("type to filter"),
        "the confirm box swallowed the stray keys: {text:?}"
    );
    assert!(
        !text.contains("xx"),
        "…and they never reached the filter: {text:?}"
    );
    assert!(
        core.borrow().last_error.is_none(),
        "consent is not an error state"
    );
}

#[test]
fn declining_the_offer_installs_nothing() {
    let (program, screen, core, task_tx, mut task_rx) = start();
    drive_check(
        program,
        &screen,
        &task_tx,
        Command::NO,
        Outcome::Available(release(Some("https://example/tvdocker_9.9.9.tar.gz"))),
    );
    assert!(task_rx.try_recv().is_err(), "No means no download, no swap");
    assert!(
        core.borrow().last_error.is_none(),
        "a declined offer is quiet"
    );
}

#[test]
fn a_release_without_an_asset_for_this_target_offers_no_install() {
    let (program, screen, core, task_tx, mut task_rx) = start();
    drive_check(
        program,
        &screen,
        &task_tx,
        Command::CANCEL,
        Outcome::Available(release(None)),
    );
    assert!(
        task_rx.try_recv().is_err(),
        "a page fallback cannot be installed — nothing is posted"
    );
    assert!(core.borrow().last_error.is_none());
}
