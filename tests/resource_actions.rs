#![forbid(unsafe_code)]
//! Action flows through the real pump:
//! right-click targeting, delete confirmation, the shell round trip,
//! the run form, F2 → Logs.
//!
//! **Modal tests drive `app::run_program`, not bare `pump_once`.** tvision-rs
//! answers a view's `request_message_box`/`request_exec_view` by stashing the
//! built dialog into `Program::pending_modal`, which only the private
//! `pump_and_drive` (run under `run_app`) executes — `pump_once` leaves it
//! pending forever, so the box would never appear. The pattern is
//! `tests/chrome.rs::f1_help_documents_the_table_sort_keys`: pre-queue every
//! key, close the box with a command it handles, then end the drive with
//! [`common::close_modal_and_quit`] (`0.17`: a modal alone receives keys and
//! commands, so a bare `QUIT` is *dropped* while a box is up). An open modal
//! swallows its keys, which is what proves it existed.
mod common;

use common::{SharedClock, close_modal_and_quit, container_state, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::Command;
use tv::event::{Event, Key, KeyModifiers, MouseButtons, MouseEvent};
use tv::{HeadlessBackend, HeadlessHandle, Point, Program};
use tvdocker::app;
use tvdocker::core::{AppCore, AppMsg, TaskReq};
use tvdocker::domain::docker::containers::ContainerSummary;

type TaskRx = tokio::sync::mpsc::UnboundedReceiver<TaskReq>;

fn image_row(id: &str, repo: &str, tag: &str) -> tvdocker::domain::docker::images::ImageEntry {
    tvdocker::domain::docker::images::ImageEntry {
        id: id.into(),
        repository: repo.into(),
        tag: tag.into(),
        size: 1_000,
    }
}

fn network_row(id: &str, name: &str) -> tvdocker::domain::docker::networks::NetworkEntry {
    tvdocker::domain::docker::networks::NetworkEntry {
        id: id.into(),
        name: name.into(),
        driver: "bridge".into(),
        scope: "local".into(),
        subnet: String::new(),
        gateway: String::new(),
        containers: 0,
    }
}

fn volume_row(name: &str) -> tvdocker::domain::docker::volumes::VolumeEntry {
    tvdocker::domain::docker::volumes::VolumeEntry {
        name: name.into(),
        driver: "local".into(),
        mountpoint: format!("/var/lib/docker/volumes/{name}/_data"),
        size: 1_000,
    }
}

/// Real `build()` so the desktop owns a REAL task channel whose
/// receiver this test keeps — every action asserts on the `TaskReq`s.
fn start(rows: Vec<ContainerSummary>) -> (Program, HeadlessHandle, Rc<RefCell<AppCore>>, TaskRx) {
    // Hermetic: F4's shell dialog reads/writes the config, so point it at a
    // temp file — never the developer's real `~/.config/tvdocker/`.
    common::isolate_config();
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, _clock) = SharedClock::new(0);
    let core = Rc::new(RefCell::new(AppCore::new()));
    core.borrow_mut().apply(AppMsg::Containers(rows));
    let (task_tx, task_rx) = tokio::sync::mpsc::unbounded_channel();
    let (mut program, _app_tx, _action_tx) = app::build(
        Box::new(backend),
        Box::new(shared),
        Rc::clone(&core),
        None,
        task_tx,
    );
    pump(&mut program, 5); // startup + status priming (chords precedent)
    (program, screen, core, task_rx)
}

fn two_rows() -> Vec<ContainerSummary> {
    vec![
        container_state("id-alpha", "webalpha", "running"),
        container_state("id-beta", "webbeta", "exited"),
    ]
}

/// One live stats sample for `id-web` (the stats poller keys its rows by
/// container, so the row carries the container id).
fn stats_row(id: &str, name: &str) -> tvdocker::domain::docker::statistics::StatEntry {
    tvdocker::domain::docker::statistics::StatEntry {
        id: id.into(),
        name: name.into(),
        cpu_percent: 9.9,
        memory_usage: 104_857_600,
        memory_limit: 524_288_000,
        memory_percent: 20.0,
        net_rx: 1_000_000,
        net_tx: 2_000_000,
        block_read: 0,
        block_write: 0,
        pids: 4,
    }
}

/// The Statistics window, opened on a live stats row: the same startup
/// as [`start`], plus the stats the window renders.
fn start_with_stats() -> (Program, HeadlessHandle, Rc<RefCell<AppCore>>, TaskRx) {
    let (mut program, screen, core, task_rx) =
        start(vec![container_state("id-web", "web", "running")]);
    core.borrow_mut()
        .apply(AppMsg::Statistics(vec![stats_row("id-web", "web")]));
    screen.push_event(Event::Command(app::CMD_OPEN_STATISTICS));
    pump(&mut program, 10);
    (program, screen, core, task_rx)
}

/// Right-click a point in **absolute** screen coordinates.
///
/// The window sits at `Rect(2,1,78,23)` *inside* the desktop, which the
/// startup factory insets by one row for the menu bar — so on screen the
/// frame starts at y=2: title 2, info 3, filter 4, header 5, row1 6,
/// row2 7. (An off-by-one here silently targets the row above.)
fn right_click(screen: &HeadlessHandle, x: i32, y: i32) {
    screen.push_event(Event::MouseDown(MouseEvent {
        position: Point::new(x, y),
        buttons: MouseButtons {
            right: true,
            ..MouseButtons::default()
        },
        ..MouseEvent::default()
    }));
}

#[test]
fn right_click_menu_targets_the_clicked_row() {
    let (mut program, screen, _core, mut task_rx) = start(two_rows());
    right_click(&screen, 12, 7); // absolute y=7 ⇒ ROW 2 (webbeta, exited)
    pump(&mut program, 3);
    let text = screen_text(&screen);
    assert!(
        text.contains("Details") && text.contains("Delete") && text.contains("start"),
        "context menu open, and it is built from ROW 2's state (exited ⇒ start): {text:?}"
    );
    // Hot letter 'd' → Details (menus are hot-letter scoped,).
    screen.push_key(Key::Char('d'), KeyModifiers::default());
    pump(&mut program, 5);
    match task_rx.try_recv() {
        Ok(TaskReq::Inspect { id }) => assert_eq!(id, "id-beta", "targets the clicked row"),
        _ => panic!("Inspect task for the clicked row"),
    }
    // The Details window opened for the clicked row (its info row still
    // reads "Loading…" — the inspect payload comes from the worker, which
    // this harness does not run; the rendered payload is
    // `views::details`'s own test).
    let text = screen_text(&screen);
    assert!(text.contains("Details"), "details window opened: {text:?}");
    assert!(
        text.contains("Loading container details"),
        "loading state: {text:?}"
    );
}

/// The (row, col) of the first `needle` occurrence in the screen text.
fn at(text: &str, needle: &str) -> (i32, i32) {
    for (y, line) in text.lines().enumerate() {
        if let Some(x) = line.find(needle) {
            return (y as i32, x as i32);
        }
    }
    panic!("{needle:?} is not on screen: {text:?}");
}

/// Shift+F10 is the mouse-free context menu: same items and same target
/// rule (the CURSOR row), anchored at that row's cell.
#[test]
fn shift_f10_opens_the_context_menu_for_the_cursor_row() {
    let (mut program, screen, _core, _task_rx) = start(two_rows());
    screen.push_key(Key::Down, KeyModifiers::default()); // ROW 2 (webbeta, exited)
    pump(&mut program, 2);
    screen.push_key(
        Key::F(10),
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    let text = screen_text(&screen);
    assert!(
        text.contains("Details") && text.contains("Delete") && text.contains("start"),
        "context menu open, built from the CURSOR row's state (exited ⇒ start): {text:?}"
    );
    // Placement: the popup opens below-right of the anchor, so its FIRST
    // item sits two rows under the row it was opened for.
    let (row_y, row_x) = at(&text, "webbeta");
    let (item_y, item_x) = at(&text, "Explorer");
    assert_eq!(
        item_y,
        row_y + 2,
        "the menu opens AT the cursor row: {text:?}"
    );
    assert!(
        item_x.abs_diff(row_x) <= 2,
        "…and at its left edge (row x={row_x}, menu x={item_x}): {text:?}"
    );
}

/// Shift+F10 opens the ROW menu and never arms the menu bar — that is
/// plain F10's binding, and tvision matches key events including
/// modifiers.
#[test]
fn shift_f10_opens_the_row_menu_not_the_menu_bar() {
    let (mut program, screen, _core, mut task_rx) = start(two_rows());
    screen.push_key(
        Key::F(10),
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    let text = screen_text(&screen);
    assert!(text.contains("Details"), "the row menu is open: {text:?}");
    screen.push_key(Key::Esc, KeyModifiers::default());
    pump(&mut program, 4);
    // Enter still means Details for the cursor row: had Shift+F10 armed
    // the menu bar (plain F10's binding), this Enter would instead open
    // the bar's File popup (`chrome.rs::f10_then_enter_opens_the_file_menu`
    // pins that route).
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 5);
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::Inspect { id }) if id == "id-alpha"),
        "Enter still opens Details — the menu bar was never armed"
    );
}

/// The Statistics window carries the Containers row menu: Shift+F10 on a
/// stats row opens the same items, and they act on that row's CONTAINER
/// (the desktop's openers queue container ids, not names).
#[test]
fn the_stats_row_menu_is_the_containers_menu() {
    let (mut program, screen, _core, mut task_rx) = start_with_stats();
    screen.push_key(
        Key::F(10),
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    let text = screen_text(&screen);
    assert!(
        text.contains("Explorer")
            && text.contains("Image")
            && text.contains("Details")
            && text.contains("Delete"),
        "the Containers menu on the stats row: {text:?}"
    );
    // 'd' → Details → an inspect of the ROW's container.
    screen.push_key(Key::Char('d'), KeyModifiers::default());
    pump(&mut program, 5);
    match task_rx.try_recv() {
        Ok(TaskReq::Inspect { id }) => assert_eq!(id, "id-web", "the row's container"),
        _ => panic!("Inspect task for the stats row"),
    }
}

/// A stats row is a RUNNING container by construction (docker stats only
/// samples running ones), so its menu offers Stop — even when the
/// containers list has not caught up (it polls separately).
#[test]
fn the_stats_menu_offers_stop_for_its_running_container() {
    let (mut program, screen, core, _task_rx) = start_with_stats();
    // The containers list lags behind (or lost the row meanwhile): the
    // stats row itself still knows its container is running.
    core.borrow_mut().apply(AppMsg::Containers(vec![]));
    pump(&mut program, 3);
    screen.push_key(
        Key::F(10),
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    let text = screen_text(&screen);
    let lower = text.to_lowercase();
    assert!(
        lower.contains("stop") && !lower.contains("start"),
        "a stats row is running ⇒ Stop, never start: {text:?}"
    );
}

/// The Image jump works from a stats row too: the menu's **Image** opens
/// the Images window on THAT row's container image (proved by the cursor:
/// Del in the Images window must remove the container's image, not the
/// first row's).
#[test]
fn the_stats_menu_image_jump_opens_the_container_image() {
    let (mut program, screen, core, mut task_rx) = start_with_stats();
    core.borrow_mut().apply(AppMsg::Images(vec![
        image_row("sha256:other", "zzz", "1"),
        image_row("sha256:img", "nginx", "1.27"),
    ]));
    pump(&mut program, 3);
    screen.push_key(
        Key::F(10),
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    screen.push_key(Key::Char('i'), KeyModifiers::default()); // ~I~mage
    pump(&mut program, 16); // the Images status def lands ~9 pumps after open
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(
        matches!(
            task_rx.try_recv(),
            Ok(TaskReq::RemoveImage { id }) if id == "sha256:img"
        ),
        "the jump moved the Images cursor to the stats row's container image"
    );
}

#[test]
fn enter_opens_details_for_the_cursor_row() {
    let (mut program, screen, _core, mut task_rx) = start(two_rows());
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 5);
    match task_rx.try_recv() {
        Ok(TaskReq::Inspect { id }) => assert_eq!(id, "id-alpha", "cursor row"),
        _ => panic!("Inspect task"),
    }
    let text = screen_text(&screen);
    assert!(text.contains("Details"), "details window opened: {text:?}");
    // No worker runs in this harness, so the payload is still empty — the
    // window must be in loading state (its rendered state
    // with data is covered by `views::details`'s own tests).
    assert!(
        text.contains("Loading container details"),
        "loading state until the inspect answers: {text:?}"
    );
}

#[test]
fn details_reopens_after_being_closed() {
    let (mut program, screen, _core, mut task_rx) = start(two_rows());
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 5);
    match task_rx.try_recv() {
        Ok(TaskReq::Inspect { id }) => assert_eq!(id, "id-alpha"),
        _ => panic!("Inspect task"),
    }
    assert!(screen_text(&screen).contains("Details"), "opened");

    // Close the focused Details window. Its Drop clears the slot's open
    // flag, so the next Enter must build a FRESH window — before the fix
    // the stale flag + dead ViewId turned Enter into a silent no-op.
    screen.push_event(Event::Command(Command::CLOSE));
    pump(&mut program, 5);
    assert!(
        !screen_text(&screen).contains("Details"),
        "closed: {:?}",
        screen_text(&screen)
    );

    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 5);
    match task_rx.try_recv() {
        Ok(TaskReq::Inspect { id }) => assert_eq!(id, "id-alpha", "re-inspected"),
        _ => panic!("Inspect task on reopen"),
    }
    assert!(
        screen_text(&screen).contains("Details"),
        "reopened: {:?}",
        screen_text(&screen)
    );
}

#[test]
fn each_container_keeps_its_own_details_window() {
    let (mut program, screen, _core, mut task_rx) = start(two_rows());
    // Row 1 → Details.
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 5);
    match task_rx.try_recv() {
        Ok(TaskReq::Inspect { id }) => assert_eq!(id, "id-alpha"),
        _ => panic!("Inspect task"),
    }
    // Refocus Containers (its open-or-focus command), move to row 2 and
    // open its Details too — without closing the first. Re-focusing a
    // window whose body is the nested upstream `Table` queues focus
    // events, so the refocus settles late (~10 pumps); moving earlier
    // would send `Down` to the still-focused Details window.
    screen.push_event(Event::Command(app::CMD_OPEN_CONTAINERS));
    pump(&mut program, 10);
    screen.push_key(Key::Down, KeyModifiers::default());
    pump(&mut program, 2);
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 5);
    match task_rx.try_recv() {
        Ok(TaskReq::Inspect { id }) => assert_eq!(id, "id-beta", "row 2"),
        _ => panic!("Inspect task for row 2"),
    }
    // Two windows are alive: closing the focused one leaves the first.
    // A singleton would have re-targeted the lone window and then removed
    // it leaving nothing to focus next. Cycle focus (F6 → NEXT) to the
    // surviving Details window and read its loading row.
    screen.push_event(Event::Command(Command::CLOSE));
    pump(&mut program, 5);
    screen.push_event(Event::Command(Command::NEXT));
    pump(&mut program, 5);
    let text = screen_text(&screen);
    assert!(
        text.contains("Details — webalpha") && text.contains("Loading container details"),
        "the first container's Details window survives: {text:?}"
    );
}

#[test]
fn delete_confirm_no_sends_nothing() {
    let (program, screen, _core, mut task_rx) = start(two_rows());
    // Delete opens the confirm box; the box then SWALLOWS Down (the cursor
    // must not move) and answers 'n'. An open modal runs only under
    // `run_program`, so nothing can be inspected mid-run: cancel that box
    // and immediately open a second one with Delete + Yes. The ONLY request
    // must be the removal of the row the cursor still sits on — row 1 —
    // which proves both that No queued nothing and that the first box ate
    // the Down (an unswallowed Down would have retargeted the second delete
    // to row 2). The upstream Table paints its cursor as a row style, not a
    // `>` marker, so this indirect probe replaces the old screen-text one.
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_key(Key::Down, KeyModifiers::default());
    screen.push_event(Event::Command(Command::NO));
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    let mut got = Vec::new();
    while let Ok(req) = task_rx.try_recv() {
        got.push(req);
    }
    assert!(
        matches!(got.as_slice(), [TaskReq::RemoveContainer { id }] if id.as_str() == "id-alpha"),
        "No queued nothing, and the swallowed Down left the cursor on row 1"
    );
}

#[test]
fn delete_confirm_yes_removes_exactly_the_targets() {
    // Single: the cursor row only.
    let (program, screen, _core, mut task_rx) = start(two_rows());
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(
        matches!(
            task_rx.try_recv(),
            Ok(TaskReq::RemoveContainer { id }) if id == "id-alpha"
        ),
        "exactly the cursor row"
    );
    assert!(task_rx.try_recv().is_err(), "one target ⇒ one request");

    // Batch: Space-select both rows (Review Focus #3).
    let (program, screen, _core, mut task_rx) = start(two_rows());
    screen.push_key(Key::Char(' '), KeyModifiers::default());
    screen.push_key(Key::Down, KeyModifiers::default());
    screen.push_key(Key::Char(' '), KeyModifiers::default());
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    let mut got = Vec::new();
    while let Ok(TaskReq::RemoveContainer { id }) = task_rx.try_recv() {
        got.push(id);
    }
    got.sort();
    assert_eq!(got, vec!["id-alpha", "id-beta"], "exactly the selection");
}

#[test]
fn a_cancelled_images_delete_does_not_stain_the_next_delete() {
    // Review Focus #4: cancel an IMAGES delete (drops targets AND kind)…
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    core.borrow_mut().apply(AppMsg::Images(vec![image_row(
        "sha256:aaa",
        "nginx",
        "1.27",
    )]));
    screen.push_event(Event::Command(app::CMD_OPEN_IMAGES));
    pump(&mut program, 3);
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::NO));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(task_rx.try_recv().is_err(), "cancelled ⇒ nothing queued");

    // …then a CONTAINERS delete must still send RemoveContainer.
    let (program, screen, _core, mut task_rx) = start(two_rows());
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(
        matches!(
            task_rx.try_recv(),
            Ok(TaskReq::RemoveContainer { id }) if id == "id-alpha"
        ),
        "the kind is re-derived per action, never inherited"
    );
}

#[test]
fn f2_opens_a_logs_window_and_starts_one_session() {
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    screen.push_key(Key::F(2), KeyModifiers::default());
    pump(&mut program, 5);
    assert!(
        screen_text(&screen).contains("Logs — webalpha"),
        "logs window for the cursor row: {}",
        screen_text(&screen)
    );
    assert!(
        matches!(
            task_rx.try_recv(),
            Ok(TaskReq::StartLogs { id, .. }) if id == "id-alpha"
        ),
        "one session started"
    );
    assert!(
        core.borrow().logs.contains_key("id-alpha"),
        "the desktop created the session buffer FIRST"
    );
    // Second F2 → focus, NO second session.
    screen.push_key(Key::F(2), KeyModifiers::default());
    pump(&mut program, 4);
    assert!(task_rx.try_recv().is_err(), "open-or-focus, not reopen");
}

#[test]
fn shell_submit_sends_the_dialog_values_for_the_pending_container() {
    let (program, screen, _core, mut task_rx) = start(two_rows());
    // an UNEDITED submit must never touch the user's config
    // file — not even to create it. Snapshot it around the round trip.
    let cfg = tvdocker::domain::config::TvdockerConfig::path();
    let cfg_before = std::fs::read_to_string(&cfg).ok();
    // F4 opens the config dialog (modal ⇒ run_program), then the dialog
    // is confirmed with `Command::OK` — the idiom tvision-rs's own tests
    // use to drive a modal from a pre-queued event stream. A live human
    // Enter DOES submit (the idle pass re-targets the status line to the
    // modal's NO_CONTEXT ctx — see
    // dialogs::shell_form_tests::both_dialogs_report_no_help_ctx…), but
    // this pre-queued stream never idles between events, so the status
    // line still holds the Containers def and a raw Enter would be
    // pre-routed to CMD_REQ_DETAILS instead of reaching the dialog.
    // Submitted
    // UNEDITED so `save_if_changed` sees no delta — the test must never
    // write the user's real config.
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::OK));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    match task_rx.try_recv() {
        Ok(TaskReq::StartShell {
            id,
            shell,
            user,
            workdir,
            ..
        }) => {
            assert_eq!(id, "id-alpha", "the pending container");
            let prefill = tvdocker::views::dialogs::ShellForm::prefill("webalpha", "img");
            assert_eq!(shell, prefill.shell, "chain value");
            assert_eq!(user, prefill.user);
            assert_eq!(workdir, prefill.workdir);
        }
        _ => panic!("StartShell task — the dialog must have opened and submitted"),
    }
    assert!(
        screen_text(&screen).contains("shell @ webalpha"),
        "PTY window opened: {}",
        screen_text(&screen)
    );
    assert_eq!(
        std::fs::read_to_string(&cfg).ok(),
        cfg_before,
        "an unedited submit must never touch the user's config"
    );
}

#[test]
fn shell_cancel_sends_nothing() {
    let (program, screen, _core, mut task_rx) = start(two_rows());
    // The SAME F4 path as the submit test — paired, so the empty channel
    // here proves Esc cancelled rather than the dialog never opening.
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_key(Key::Esc, KeyModifiers::default());
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(task_rx.try_recv().is_err(), "cancel ⇒ no exec");
    assert!(!screen_text(&screen).contains("shell @"), "no window");
}

/// F4 → shell → close the shell window → F4 again: the config dialog
/// must open again.
///
/// The shell window has to clear its slot's open flag when it is closed
/// (the Drop pattern every other window follows). If it does not, the
/// desktop still believes a shell is open and "focuses" the DEAD
/// window id — `request_focus` on a removed view is a silent no-op — so
/// F4 does nothing at all: no config dialog, no window.
#[test]
fn f4_reopens_the_shell_config_after_the_shell_window_was_closed() {
    let (program, screen, _core, mut task_rx) = start(two_rows());
    // F4 → the config dialog → OK ⇒ the shell window opens.
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::OK));
    // F3 closes the focused window (the shell window).
    screen.push_key(Key::F(3), KeyModifiers::default());
    // F4 again ⇒ the config dialog must open again (and submit ⇒ a
    // SECOND exec).
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::OK));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    let mut shells = 0;
    while let Ok(TaskReq::StartShell { .. }) = task_rx.try_recv() {
        shells += 1;
    }
    assert_eq!(
        shells, 2,
        "F4 after closing the shell window must open the config dialog again"
    );
}

/// Exactly one shell open for the container ⇒ F4 must ASK before opening
/// another: Yes ⇒ the config dialog ⇒ a second exec. The prompt's own
/// text cannot be asserted (a modal only exists mid-`run_program`, which
/// drives to quit), so the proof is the `StartShell` count.
#[test]
fn f4_with_one_shell_open_asks_before_opening_another() {
    let (program, screen, _core, mut task_rx) = start(two_rows());
    // Shell #1: F4 → config dialog → OK.
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::OK));
    // The shell window swallowed focus — back to the table, F4 again
    // with exactly one shell open ⇒ the yes/no prompt ⇒ Yes ⇒ the
    // config dialog ⇒ OK ⇒ a second exec.
    screen.push_event(Event::Command(app::CMD_OPEN_CONTAINERS));
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    screen.push_event(Event::Command(Command::OK));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    let mut shells = 0;
    while let Ok(TaskReq::StartShell { .. }) = task_rx.try_recv() {
        shells += 1;
    }
    assert_eq!(shells, 2, "Yes ⇒ a second PTY opens");
}

/// The prompt's No must queue NOTHING (it shows the current shell), and
/// the next F4 must ask again — still exactly one open — where Yes then
/// opens the second shell. No alone cannot fail before the prompt exists
/// (focus-not-open also yields one exec), so No and the re-ask share one
/// stream whose second answer makes it red.
#[test]
fn f4_prompt_no_shows_the_current_shell_and_the_next_asks_again() {
    let (program, screen, _core, mut task_rx) = start(two_rows());
    // Shell #1.
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::OK));
    // F4 ⇒ prompt ⇒ No ⇒ no new exec.
    screen.push_event(Event::Command(app::CMD_OPEN_CONTAINERS));
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::NO));
    // F4 ⇒ prompt AGAIN (one shell still open) ⇒ Yes ⇒ dialog ⇒ OK.
    screen.push_event(Event::Command(app::CMD_OPEN_CONTAINERS));
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    screen.push_event(Event::Command(Command::OK));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    let mut shells = 0;
    while let Ok(TaskReq::StartShell { .. }) = task_rx.try_recv() {
        shells += 1;
    }
    assert_eq!(
        shells, 2,
        "No opened nothing; the next F4 asked again and Yes opened the second"
    );
}

/// Two shells open ⇒ F4 asks NO MORE: the dialog opens directly. The
/// third exec rides the queued OK — if a prompt were up instead, it would
/// swallow the OK as a non-Yes answer and no third exec would happen.
#[test]
fn f4_with_several_shells_open_opens_another_without_asking() {
    let (program, screen, _core, mut task_rx) = start(two_rows());
    // Shell #1.
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::OK));
    // Shell #2 through the prompt (the only way past "exactly one").
    screen.push_event(Event::Command(app::CMD_OPEN_CONTAINERS));
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    screen.push_event(Event::Command(Command::OK));
    // Third F4 with two open: straight to the config dialog.
    screen.push_event(Event::Command(app::CMD_OPEN_CONTAINERS));
    screen.push_key(Key::F(4), KeyModifiers::default());
    screen.push_event(Event::Command(Command::OK));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    let mut shells = 0;
    while let Ok(TaskReq::StartShell { .. }) = task_rx.try_recv() {
        shells += 1;
    }
    assert_eq!(shells, 3, "no prompt at two open — a third PTY opens");
}

#[test]
fn images_del_batch_removes_exactly_the_selection() {
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    core.borrow_mut().apply(AppMsg::Images(vec![
        image_row("sha256:aaa", "nginx", "1.27"),
        image_row("sha256:bbb", "redis", "7"),
    ]));
    screen.push_event(Event::Command(app::CMD_OPEN_IMAGES));
    pump(&mut program, 3);
    screen.push_key(Key::Char(' '), KeyModifiers::default());
    screen.push_key(Key::Down, KeyModifiers::default());
    screen.push_key(Key::Char(' '), KeyModifiers::default());
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    let mut got = Vec::new();
    while let Ok(TaskReq::RemoveImage { id }) = task_rx.try_recv() {
        got.push(id);
    }
    got.sort();
    assert_eq!(
        got,
        vec!["sha256:aaa", "sha256:bbb"],
        "exactly the selection"
    );
}

#[test]
fn networks_del_confirms_then_sends_remove_network() {
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    core.borrow_mut()
        .apply(AppMsg::Networks(vec![network_row("net1", "bridge")]));
    screen.push_event(Event::Command(app::CMD_OPEN_NETWORKS));
    pump(&mut program, 3);
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(
        matches!(
            task_rx.try_recv(),
            Ok(TaskReq::RemoveNetwork { id }) if id == "net1"
        ),
        "the network row id travels to the worker"
    );
}

#[test]
fn volumes_menu_delete_sends_remove_volume() {
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    core.borrow_mut()
        .apply(AppMsg::Volumes(vec![volume_row("data")]));
    screen.push_event(Event::Command(app::CMD_OPEN_VOLUMES));
    pump(&mut program, 3);
    // Right-click the row, hot-letter 'x' → Delete.
    right_click(&screen, 12, 7); // the cascaded window sits one row lower
    pump(&mut program, 3);
    screen.push_key(Key::Char('x'), KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(
        matches!(
            task_rx.try_recv(),
            Ok(TaskReq::RemoveVolume { name }) if name == "data"
        ),
        "volumes are keyed by NAME"
    );
}

#[test]
fn images_menu_prune_confirms_and_queues_prune_once() {
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    core.borrow_mut().apply(AppMsg::Images(vec![image_row(
        "sha256:aaa",
        "nginx",
        "1.27",
    )]));
    screen.push_event(Event::Command(app::CMD_OPEN_IMAGES));
    pump(&mut program, 3);
    right_click(&screen, 12, 7); // the cascaded window sits one row lower
    pump(&mut program, 3);
    screen.push_key(Key::Char('p'), KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::PruneImages)),
        "one prune request"
    );
    assert!(task_rx.try_recv().is_err(), "exactly one");
}

#[test]
fn images_menu_dangling_confirms_and_queues_the_sweep_once() {
    // Pin the dangling desktop flow (not just the menu label); mirror
    // the prune test. No worker runs in this harness, so the
    // host-mutating sweep never executes — only the queued request is
    // asserted.
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    core.borrow_mut().apply(AppMsg::Images(vec![image_row(
        "sha256:aaa",
        "nginx",
        "1.27",
    )]));
    screen.push_event(Event::Command(app::CMD_OPEN_IMAGES));
    pump(&mut program, 3);
    right_click(&screen, 12, 7); // the cascaded window sits one row lower
    pump(&mut program, 3);
    screen.push_key(Key::Char('d'), KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::RemoveDanglingImages)),
        "one sweep request"
    );
    assert!(task_rx.try_recv().is_err(), "exactly one");
}

#[test]
fn images_f2_opens_the_run_form_and_submits_its_values() {
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    core.borrow_mut().apply(AppMsg::Images(vec![image_row(
        "sha256:aaa",
        "nginx",
        "1.27",
    )]));
    screen.push_event(Event::Command(app::CMD_OPEN_IMAGES));
    // The status def switches on an IDLE pass; measured: the Images def
    // ("F2 Run…") lands at the 9th pump after the open command (the nested
    // upstream Table queues focus/broadcast events, so the help ctx settles
    // later than before — the chrome.rs chords tests pin the same settling).
    pump(&mut program, 10);
    let last = screen_text(&screen)
        .lines()
        .last()
        .unwrap_or("")
        .to_string();
    assert!(last.contains("F2 Run…"), "Images def active: {last}");
    screen.push_key(Key::F(2), KeyModifiers::default());
    screen.push_event(Event::Command(Command::OK));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    match task_rx.try_recv() {
        Ok(TaskReq::RunContainer { opts }) => {
            assert_eq!(opts.image, "nginx:1.27", "repository:tag from the row");
            assert_eq!(opts.cmd, "", "unedited form");
        }
        Ok(TaskReq::Inspect { .. }) => panic!("wrong variant: Inspect"),
        Ok(TaskReq::RemoveContainer { .. }) => panic!("wrong variant: RemoveContainer"),
        Ok(TaskReq::SetContainerState { .. }) => panic!("wrong variant: SetContainerState"),
        Ok(TaskReq::StartLogs { .. }) => panic!("wrong variant: StartLogs"),
        Ok(TaskReq::StartShell { .. }) => panic!("wrong variant: StartShell"),
        Ok(_) => panic!("wrong variant: other"),
        Err(_) => panic!("RunContainer task (channel empty)"),
    }
    assert!(task_rx.try_recv().is_err(), "exactly one");
}

#[test]
fn run_submit_unedited_never_writes_the_config() {
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    let cfg = tvdocker::domain::config::TvdockerConfig::path();
    let before = std::fs::read_to_string(&cfg).ok();
    core.borrow_mut().apply(AppMsg::Images(vec![image_row(
        "sha256:aaa",
        "nginx",
        "1.27",
    )]));
    screen.push_event(Event::Command(app::CMD_OPEN_IMAGES));
    // The Images def lands at pump 9 (measured — see images_f2…). At
    // pump 3 F2 would hit the stale Containers def and open Logs
    // instead, making every assertion below VACUOUS (reviewer finding 2).
    pump(&mut program, 10);
    let last = screen_text(&screen)
        .lines()
        .last()
        .unwrap_or("")
        .to_string();
    assert!(last.contains("F2 Run…"), "Images def active: {last}");
    screen.push_key(Key::F(2), KeyModifiers::default());
    screen.push_event(Event::Command(Command::OK));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    // The form really opened and submitted — this is the run path.
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::RunContainer { .. })),
        "the run form opened and submitted (the config assertion is not vacuous)"
    );
    assert_eq!(
        std::fs::read_to_string(&cfg).ok(),
        before,
        "an unedited submit must never touch the user's config"
    );
    // The NON-virgin no-write cases (existing config, missing entry,
    // workdir preservation) are pinned by the pure `persist_row` unit
    // tests — this shared per-process config must stay untouched.
}

#[test]
fn run_form_esc_sends_nothing() {
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    core.borrow_mut().apply(AppMsg::Images(vec![image_row(
        "sha256:aaa",
        "nginx",
        "1.27",
    )]));
    screen.push_event(Event::Command(app::CMD_OPEN_IMAGES));
    pump(&mut program, 10); // the Images def lands at pump 9 (measured)
    screen.push_key(Key::F(2), KeyModifiers::default());
    // The form SWALLOWS Esc (the modal gate) — the swallowed key is what
    // proves it was really up; without the form, Esc would close the
    // Images window instead.
    screen.push_key(Key::Esc, KeyModifiers::default());
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(task_rx.try_recv().is_err(), "cancel ⇒ no run");
    let text = screen_text(&screen);
    assert!(
        text.contains("Images"),
        "the Images window SURVIVED — Esc was swallowed by the run form \
         (without the form, Esc's chain closes the active window): {text:?}"
    );
}

/// The container fixtures' image is `sha256:img`; the list puts it SECOND,
/// so a cursor that never moved would target `sha256:other` instead. Del
/// acts on the cursor row ⇒ the `RemoveImage` id is the observable proof
/// that the jump put the cursor on the container's image.
#[test]
fn image_menu_jump_opens_images_with_the_container_image_under_the_cursor() {
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    core.borrow_mut().apply(AppMsg::Images(vec![
        image_row("sha256:other", "zzz", "1"),
        image_row("sha256:img", "nginx", "1.27"),
    ]));
    right_click(&screen, 12, 6); // ROW 1 (webalpha)
    pump(&mut program, 3);
    screen.push_key(Key::Char('i'), KeyModifiers::default()); // ~I~mage
    pump(&mut program, 16); // the Images status def lands ~9 pumps after open
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(
        matches!(
            task_rx.try_recv(),
            Ok(TaskReq::RemoveImage { id }) if id == "sha256:img"
        ),
        "Del targeted the container's image row — the jump moved the cursor"
    );
}

/// A pruned image (no row to jump to) must not swallow the request: the
/// Images window still opens.
#[test]
fn image_menu_jump_still_opens_images_when_the_image_is_gone() {
    let (mut program, screen, _core, _task_rx) = start(two_rows());
    // No images seeded at all — the container's image is not in the list.
    right_click(&screen, 12, 6); // ROW 1 (webalpha)
    pump(&mut program, 3);
    screen.push_key(Key::Char('i'), KeyModifiers::default()); // ~I~mage
    pump(&mut program, 10);
    let text = screen_text(&screen);
    assert!(
        text.contains("Images"),
        "the Images window opened anyway: {text:?}"
    );
}

/// The jump with the Images window ALREADY open: the desktop focuses the
/// existing window (no second one) and the open window applies the focus
/// request — same cursor proof via Del.
#[test]
fn image_menu_jump_moves_the_cursor_in_an_already_open_images_window() {
    let (mut program, screen, core, mut task_rx) = start(two_rows());
    core.borrow_mut().apply(AppMsg::Images(vec![
        image_row("sha256:other", "zzz", "1"),
        image_row("sha256:img", "nginx", "1.27"),
    ]));
    screen.push_event(Event::Command(app::CMD_OPEN_IMAGES));
    pump(&mut program, 12); // Images window open + def landed
    // Jump from the Containers window (F6 Next cycles back to it).
    screen.push_key(
        Key::F(6),
        KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 3);
    right_click(&screen, 12, 6); // ROW 1 (webalpha)
    pump(&mut program, 3);
    screen.push_key(Key::Char('i'), KeyModifiers::default()); // ~I~mage
    pump(&mut program, 16);
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(
        matches!(
            task_rx.try_recv(),
            Ok(TaskReq::RemoveImage { id }) if id == "sha256:img"
        ),
        "the cursor moved to row 2 in the already-open window (it started \
         on row 1 = sha256:other — an unmoved cursor would target that)"
    );
}
