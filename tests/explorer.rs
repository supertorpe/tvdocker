#![forbid(unsafe_code)]
//! The Explorer openers, the queued requests, and
//! the window-owned Del confirm — through the real pump.
mod common;

use common::{close_modal_and_quit, container, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::Command;
use tv::event::{Event, Key, KeyModifiers, MouseButtons, MouseEvent};
use tv::{HeadlessHandle, Point, Program};
use tvdocker::app;
use tvdocker::core::{AppCore, AppMsg, ExplorerTarget, TaskReq};
use tvdocker::domain::docker::volumes::VolumeEntry;

type TaskRx = tokio::sync::mpsc::UnboundedReceiver<TaskReq>;

fn volume(name: &str) -> VolumeEntry {
    VolumeEntry {
        name: name.into(),
        driver: "local".into(),
        mountpoint: format!("/var/lib/docker/volumes/{name}/_data"),
        size: 1_000,
    }
}

fn start() -> (Program, HeadlessHandle, Rc<RefCell<AppCore>>, TaskRx) {
    let h = common::app_start(|core| {
        core.borrow_mut()
            .apply(AppMsg::Containers(vec![container("id-a", "webalpha")]));
    });
    (h.program, h.screen, h.core, h.task_rx)
}

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
fn explorer_opens_from_the_containers_menu_and_lists_both_sides() {
    let (mut program, screen, _core, mut task_rx) = start();
    right_click(&screen, 12, 6); // row 0
    pump(&mut program, 3);
    screen.push_key(Key::Char('e'), KeyModifiers::default()); // ~E~xplorer
    pump(&mut program, 4);
    let text = screen_text(&screen);
    assert!(
        text.contains("host filter") || text.contains("remote filter") || text.contains("Explorer"),
        "the window opened for the clicked container: {text:?}"
    );
    // Two initial listings were requested (host + container).
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::ExplorerList { .. })),
        "host listing requested"
    );
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::ExplorerList { .. })),
        "container listing requested"
    );
}

#[test]
fn volumes_enter_opens_the_volume_explorer() {
    let (mut program, screen, core, mut task_rx) = start();
    core.borrow_mut()
        .apply(AppMsg::Volumes(vec![volume("data")]));
    screen.push_event(Event::Command(app::CMD_OPEN_VOLUMES));
    pump(&mut program, 6); // the volumes def lands late (4A measured)
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 4);
    let text = screen_text(&screen);
    assert!(
        text.contains("host filter") || text.contains("remote filter") || text.contains("Explorer"),
        "the volume explorer opened: {text:?}"
    );
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::ExplorerList { .. })),
        "the host listing is requested"
    );
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::ExplorerList { target: ExplorerTarget::Volume(name), .. }) if name == "data"),
        "the remote listing names the volume (the helper is created by the WORKER)"
    );
}

#[test]
fn explorer_del_confirm_queues_the_task_only_on_yes() {
    let (mut program, screen, core, mut task_rx) = start();
    right_click(&screen, 12, 6);
    pump(&mut program, 3);
    screen.push_key(Key::Char('e'), KeyModifiers::default());
    pump(&mut program, 4);
    while task_rx.try_recv().is_ok() {} // drain the two build requests
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    core.borrow_mut().apply(AppMsg::ExplorerDir {
        key: "id-a".into(),
        target: ExplorerTarget::Host,
        path: home,
        entries: vec![tvdocker::domain::docker::explorer::ExplorerEntry {
            name: "a.txt".into(),
            is_dir: false,
            size: 1,
            modified: "Oct 11 09:00".into(),
            permissions: "-rw-r--r--".into(),
        }],
    });
    pump(&mut program, 5);
    for _ in 0..5 {
        screen.push_key(Key::Down, KeyModifiers::default());
        pump(&mut program, 2);
    }
    screen.push_key(Key::Delete, KeyModifiers::default());
    screen.push_event(Event::Command(Command::YES));
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::ExplorerDelete { ref paths, .. })
            if paths.len() == 1 && paths[0].ends_with("a.txt")),
        "the YES answer queues exactly the cursor row"
    );
    assert!(task_rx.try_recv().is_err(), "exactly one");
}

/// Regression: the window opens with the HOST table focused. A non-modal
/// window's `View::reset_current` is never called (only `exec_view` calls
/// it, for modals), so the pump's `settle_currency` used to land on the
/// last-inserted selectable child — the REMOTE (empty) table — and every
/// Down/Enter went to the wrong panel.
#[test]
fn explorer_opens_with_the_host_table_focused() {
    let (mut program, screen, core, mut task_rx) = start();
    right_click(&screen, 12, 6);
    pump(&mut program, 3);
    screen.push_key(Key::Char('e'), KeyModifiers::default());
    pump(&mut program, 4);
    while task_rx.try_recv().is_ok() {} // drain the two build requests
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    core.borrow_mut().apply(AppMsg::ExplorerDir {
        key: "id-a".into(),
        target: ExplorerTarget::Host,
        path: home.clone(),
        entries: vec![tvdocker::domain::docker::explorer::ExplorerEntry {
            name: "sub".into(),
            is_dir: true,
            size: 0,
            modified: "Oct 11 09:00".into(),
            permissions: "drwxr-xr-x".into(),
        }],
    });
    pump(&mut program, 5);
    // Down → row 1 (`sub`), Enter → descend into $HOME/sub. With the bug
    // the Down lands on the remote table, so the cursor stays on `..` and
    // Enter climbs to $HOME's parent instead.
    screen.push_key(Key::Down, KeyModifiers::default());
    pump(&mut program, 2);
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 2);
    close_modal_and_quit(&screen);
    app::run_program(program, None);
    let expected = format!("{}/sub", home.trim_end_matches('/'));
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::ExplorerList { target: ExplorerTarget::Host, ref path, .. }) if path == &expected),
        "Down+Enter descended on the host panel (initial focus) — expected {expected:?}"
    );
}

#[test]
fn explorer_close_clears_the_slot_so_it_reopens() {
    let (mut program, screen, _core, mut task_rx) = start();
    right_click(&screen, 12, 6);
    pump(&mut program, 3);
    screen.push_key(Key::Char('e'), KeyModifiers::default());
    pump(&mut program, 4);
    while task_rx.try_recv().is_ok() {}
    screen.push_event(Event::Command(Command::CLOSE));
    pump(&mut program, 4);
    right_click(&screen, 12, 6);
    pump(&mut program, 3);
    screen.push_key(Key::Char('e'), KeyModifiers::default());
    pump(&mut program, 4);
    let text = screen_text(&screen);
    assert!(
        text.contains("host filter") || text.contains("remote filter") || text.contains("Explorer"),
        "a closed explorer reopens (the slot Drop cleared): {text:?}"
    );
}

/// A click on the CONTAINER/right panel activates it, and the row under the
/// click becomes the cursor — proven end-to-end by pressing Enter, which only
/// queues a relist for the RIGHT panel's path if activation really happened
/// (bug report: clicking the inactive panel did nothing).
#[test]
fn a_click_on_the_inactive_panel_activates_it() {
    let (mut program, screen, core, mut task_rx) = start();
    right_click(&screen, 12, 6);
    pump(&mut program, 3);
    screen.push_key(Key::Char('e'), KeyModifiers::default());
    pump(&mut program, 4);
    while task_rx.try_recv().is_ok() {} // drain the two build requests

    // Give the right-hand (container) panel a distinctive directory.
    core.borrow_mut().apply(AppMsg::ExplorerDir {
        key: "id-a".into(),
        target: ExplorerTarget::Container("id-a".into()),
        path: "/".into(),
        entries: vec![tvdocker::domain::docker::explorer::ExplorerEntry {
            name: "zzzdir".into(),
            is_dir: true,
            size: 0,
            modified: "Oct 11 09:00".into(),
            permissions: "drwxr-xr-x".into(),
        }],
    });
    pump(&mut program, 5);

    // Locate the rendered cell and click it — no hardcoded geometry.
    let text = screen_text(&screen);
    let (y, x) = text
        .lines()
        .enumerate()
        .find_map(|(y, l)| l.find("zzzdir").map(|x| (y as i32, x as i32)))
        .expect("the right panel rendered zzzdir");

    screen.push_event(Event::MouseDown(MouseEvent {
        position: Point::new(x, y),
        buttons: MouseButtons {
            left: true,
            ..MouseButtons::default()
        },
        ..MouseEvent::default()
    }));
    pump(&mut program, 4);

    // Enter now acts on the RIGHT panel: it descends into /zzzdir.
    screen.push_key(Key::Enter, KeyModifiers::default());
    pump(&mut program, 4);
    let req = task_rx.try_recv().expect("a relist for the clicked dir");
    assert!(
        matches!(req, TaskReq::ExplorerList { path, .. } if path == "/zzzdir"),
        "Enter acted on the panel the click activated"
    );
}

/// Two clicks on the same row through the REAL pump open it (bug report:
/// double-click did not work — it was keyed on `flags.double_click`, which
/// tvision-rs 0.16's backend never sets).
#[test]
fn a_double_click_opens_the_directory() {
    let (mut program, screen, core, mut task_rx) = start();
    right_click(&screen, 12, 6);
    pump(&mut program, 3);
    screen.push_key(Key::Char('e'), KeyModifiers::default());
    pump(&mut program, 4);
    while task_rx.try_recv().is_ok() {} // drain the two build requests

    core.borrow_mut().apply(AppMsg::ExplorerDir {
        key: "id-a".into(),
        target: ExplorerTarget::Container("id-a".into()),
        path: "/".into(),
        entries: vec![tvdocker::domain::docker::explorer::ExplorerEntry {
            name: "zzzdir".into(),
            is_dir: true,
            size: 0,
            modified: "Oct 11 09:00".into(),
            permissions: "drwxr-xr-x".into(),
        }],
    });
    pump(&mut program, 5);

    let text = screen_text(&screen);
    let (y, x) = text
        .lines()
        .enumerate()
        .find_map(|(y, l)| l.find("zzzdir").map(|x| (y as i32, x as i32)))
        .expect("the right panel rendered zzzdir");

    let click = |screen: &HeadlessHandle| {
        screen.push_event(Event::MouseDown(MouseEvent {
            position: Point::new(x, y),
            buttons: MouseButtons {
                left: true,
                ..MouseButtons::default()
            },
            ..MouseEvent::default()
        }));
    };

    click(&screen);
    pump(&mut program, 3);
    assert!(
        task_rx.try_recv().is_err(),
        "the FIRST click only activates the panel"
    );

    click(&screen);
    pump(&mut program, 4);
    let req = task_rx.try_recv().expect("the second click opens the row");
    assert!(
        matches!(req, TaskReq::ExplorerList { path, .. } if path == "/zzzdir"),
        "a double-click descends into the directory"
    );
}

/// Images → Explorer… opens a window labelled by the image and queues the
/// listing for the IMAGE target (image-explorer).
#[test]
fn image_explorer_opens_the_window_and_queues_the_listing() {
    let (mut program, screen, core, mut task_rx) = start();
    core.borrow_mut().apply(AppMsg::Images(vec![
        tvdocker::domain::docker::images::ImageEntry {
            id: "sha256:abc".into(),
            repository: "alpine".into(),
            tag: "latest".into(),
            size: 1_000,
        },
    ]));
    pump(&mut program, 5);

    // The app opens Containers at startup: switch to the Images window.
    screen.push_event(Event::Command(tvdocker::app::CMD_OPEN_IMAGES));
    pump(&mut program, 5);
    while task_rx.try_recv().is_ok() {}

    // Locate the image row and click it — no hardcoded geometry.
    let before = screen_text(&screen);
    let (y, x) = before
        .lines()
        .enumerate()
        .find_map(|(y, l)| l.find("alpine").map(|x| (y as i32, x as i32)))
        .expect("the images table rendered alpine");
    // A RIGHT click opens the context menu (and selects the clicked row).
    screen.push_event(Event::MouseDown(MouseEvent {
        position: Point::new(x, y),
        buttons: MouseButtons {
            right: true,
            ..MouseButtons::default()
        },
        ..MouseEvent::default()
    }));
    pump(&mut program, 3);
    screen.push_key(Key::Char('e'), KeyModifiers::default()); // ~E~xplorer
    pump(&mut program, 5);

    let text = screen_text(&screen);
    assert!(text.contains("Explorer"), "the window opened: {text:?}");
    assert!(
        text.contains("alpine"),
        "labelled by repository:tag: {text:?}"
    );
    let _ = task_rx.try_recv(); // the HOST panel's listing comes first
    assert!(
        matches!(
            task_rx.try_recv(),
            Ok(TaskReq::ExplorerList { target: ExplorerTarget::Image(id), path, .. })
                if id == "sha256:abc" && path == "/"
        ),
        "a listing was queued for the IMAGE target"
    );
}

/// Ctrl+E is the keyboard-only route into the image explorer — the context
/// menu is mouse-only, so without this the window needs a right-click.
#[test]
fn ctrl_e_opens_the_image_explorer() {
    let (mut program, screen, core, mut task_rx) = start();
    core.borrow_mut().apply(AppMsg::Images(vec![
        tvdocker::domain::docker::images::ImageEntry {
            id: "sha256:abc".into(),
            repository: "alpine".into(),
            tag: "latest".into(),
            size: 1_000,
        },
    ]));
    screen.push_event(Event::Command(app::CMD_OPEN_IMAGES));
    pump(&mut program, 6);
    while task_rx.try_recv().is_ok() {}

    screen.push_key(
        Key::Char('e'),
        KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        },
    );
    pump(&mut program, 5);

    let text = screen_text(&screen);
    assert!(
        text.contains("Explorer"),
        "the image explorer opened: {text:?}"
    );
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::ExplorerList { .. })),
        "the host listing is requested"
    );
    assert!(
        matches!(task_rx.try_recv(), Ok(TaskReq::ExplorerList { target: ExplorerTarget::Image(id), .. }) if id == "sha256:abc"),
        "the remote listing names the image"
    );
}
