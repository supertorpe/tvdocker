use super::*;
use crate::domain::docker::explorer::ExplorerEntry;

fn entry(name: &str, is_dir: bool) -> ExplorerEntry {
    ExplorerEntry {
        name: name.into(),
        is_dir,
        size: if is_dir { 0 } else { 10 },
        modified: "Oct 11 09:00".into(),
        permissions: "-rw-r--r--".into(),
    }
}

fn window() -> (
    ExplorerWindow,
    Rc<RefCell<AppCore>>,
    mpsc::UnboundedReceiver<TaskReq>,
) {
    let core = Rc::new(RefCell::new(AppCore::new()));
    let (tx, rx) = mpsc::unbounded_channel::<TaskReq>();
    let open = Rc::new(RefCell::new(false));
    let mut w = build(
        Rc::clone(&core),
        Rect::new(0, 0, 76, 23),
        "c1".into(),
        ExplorerTarget::Container("c1".into()),
        "web".into(),
        open,
        tx,
    );
    // The desktop program focuses the window group before children can
    // report `focused`; standalone tests recreate that (the group.rs
    // pattern): mark the group focused, then descend to the host table.
    let host_table = w.host.table;
    crate::testutil::with_ctx(|ctx| {
        w.win.state_mut().state.focused = true;
        w.win.focus_descendant(host_table, ctx);
    });
    (w, core, rx)
}

fn send(w: &mut ExplorerWindow, key: Key) {
    crate::testutil::send_key(w, key);
}

/// A left click delivered with an EXPLICIT clock: the harness's
/// `with_ctx` fixes `now_ms` at 0, so double-click timing cannot be
/// driven through it.
fn click(w: &mut ExplorerWindow, x: i32, y: i32, now_ms: u64) {
    use tv::event::MouseEvent;
    let mut out = std::collections::VecDeque::new();
    let mut timers = tv::TimerQueue::new();
    let mut deferred: Vec<tv::Deferred> = Vec::new();
    let mut ctx = Context::new(&mut out, &mut timers, now_ms, &mut deferred);
    let mut ev = Event::MouseDown(MouseEvent {
        position: tv::Point::new(x, y),
        buttons: tv::event::MouseButtons {
            left: true,
            ..tv::event::MouseButtons::default()
        },
        ..MouseEvent::default()
    });
    w.handle_event(&mut ev, &mut ctx);
}

#[test]
fn build_requests_both_initial_listings() {
    let (_w, _core, mut rx) = window();
    let first = rx.try_recv().expect("host list requested");
    assert!(
        matches!(first, TaskReq::ExplorerList { target: ExplorerTarget::Host, path, .. } if path == ExplorerWindow::host_home()),
        "the host panel lists first at $HOME"
    );
    let second = rx.try_recv().expect("remote list requested");
    assert!(
        matches!(second, TaskReq::ExplorerList { target: ExplorerTarget::Container(id), path, .. } if id == "c1" && path == "/"),
        "the remote panel lists the container root"
    );
}

#[test]
fn applied_listing_renders_rows_and_a_parent_row() {
    let (mut w, core, _rx) = window();
    core.borrow_mut().apply(crate::core::AppMsg::ExplorerDir {
        key: "c1".into(),
        target: ExplorerTarget::Container("c1".into()),
        path: "/".into(),
        entries: vec![entry("app", true), entry("notes.txt", false)],
    });
    crate::testutil::send_key(&mut w, Key::Down); // drive one event → sync
    crate::testutil::with_ctx(|ctx| w.redraw_texts(ctx));
    let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
    assert!(out.contains("app/"), "dir row with a marker: {out:?}");
    assert!(out.contains("notes.txt"), "file row: {out:?}");
    // Root has no parent row: the remote panel's rows are exactly the
    // two entries. Checked on the table itself, not the screen — the
    // HOST panel legitimately renders its own `..` row at $HOME.
    let remote_rows = w
        .win
        .child_mut(w.remote.table)
        .and_then(|v| v.as_any_mut())
        .and_then(|a| a.downcast_mut::<Table>())
        .map(|t| t.row_ids());
    assert_eq!(
        remote_rows.as_deref(),
        Some(&["app".to_string(), "notes.txt".to_string()][..]),
        "root has no parent row: {out:?}"
    );
    assert!(out.contains("2 items"), "hint counts: {out:?}");
}

#[test]
fn applied_listing_ignores_a_stale_path_response() {
    // Review Focus #1: a listing taken at another path must not
    // clobber the panel the user has since navigated to.
    let (mut w, core, _rx) = window();
    core.borrow_mut().apply(crate::core::AppMsg::ExplorerDir {
        key: "c1".into(),
        target: ExplorerTarget::Container("c1".into()),
        path: "/app".into(),
        entries: vec![entry("stale.txt", false)],
    });
    // The window is at "/" (its build path) — the /app response is
    // STALE and must be dropped.
    crate::testutil::send_key(&mut w, Key::Down);
    crate::testutil::with_ctx(|ctx| w.redraw_texts(ctx));
    let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
    assert!(!out.contains("stale.txt"), "stale listing dropped: {out:?}");
    assert!(out.contains("0 items"), "panel still empty: {out:?}");
}

#[test]
fn tab_has_exactly_two_stops_and_filters_are_escapable() {
    // Review Focus #5: Tab must hop host table ↔ remote table only
    // (the FilterBars are selectable, so the window intercepts Tab),
    // and a focused filter must be escapable via Esc.
    //
    // Table focus flags are inert in tv 0.16.0: Table's View impl
    // is `#[delegate(to = scroller)]` (table.rs:471), so the
    // focused/selected state always lands on the inner scroller and
    // `v.state().state.focused` on a Table child is ALWAYS false
    // (Ruling 21). The hops are asserted by the active-side flip and
    // by the FilterBar flags instead — the window's Tab interception
    // is exactly what must keep `focused` off the selectable bars.
    let (mut w, _core, _rx) = window();
    let focused =
        |w: &mut ExplorerWindow, id| w.win.child_mut(id).is_some_and(|v| v.state().state.focused);
    let host_filter = w.host.filter;
    let remote_filter = w.remote.filter;
    assert!(!w.active_remote, "host side active first");
    assert!(
        !focused(&mut w, host_filter) && !focused(&mut w, remote_filter),
        "opens on the host table, not a filter"
    );

    // Tab 1: host → remote.
    send(&mut w, Key::Tab);
    assert!(w.active_remote, "Tab → remote table");
    assert!(
        !focused(&mut w, host_filter) && !focused(&mut w, remote_filter),
        "Tab never stops on a filter"
    );
    // Tab 2: exactly two stops, wrapping back to the host table.
    send(&mut w, Key::Tab);
    assert!(!w.active_remote, "Tab wraps to the host table");
    assert!(
        !focused(&mut w, host_filter) && !focused(&mut w, remote_filter),
        "still exactly two stops"
    );

    // `/` focuses the active (host) filter; typing narrows; Esc
    // returns to the host table.
    send(&mut w, Key::Char('/'));
    assert!(focused(&mut w, host_filter), "/ → the host filter");
    send(&mut w, Key::Char('a'));
    let typed = w
        .win
        .child_mut(w.host.filter)
        .and_then(|v| v.as_any_mut())
        .and_then(|a| a.downcast_mut::<FilterBar>())
        .map(|f| f.value().to_string());
    assert_eq!(typed.as_deref(), Some("a"), "typing narrows the filter");
    send(&mut w, Key::Esc);
    assert!(
        !focused(&mut w, host_filter),
        "Esc returns focus to the table (table flags stay inert)"
    );
}

#[test]
fn enter_descends_backspace_climbs_and_requests_relist() {
    let (mut w, _core, mut rx) = window();
    core_seed_host(&mut w, &mut rx); // drain the two build requests
    send(&mut w, Key::Tab); // the seeded listing lives on the REMOTE panel
    w.core.borrow_mut().apply(crate::core::AppMsg::ExplorerDir {
        key: "c1".into(),
        target: ExplorerTarget::Container("c1".into()),
        path: "/".into(),
        entries: vec![entry("app", true)],
    });
    send(&mut w, Key::Down); // sync
    send(&mut w, Key::Enter); // enter app/
    let req = rx.try_recv().expect("relist for /app");
    assert!(
        matches!(req, TaskReq::ExplorerList { path, .. } if path == "/app"),
        "Enter requests the subdir"
    );
    send(&mut w, Key::Backspace);
    let req = rx.try_recv().expect("relist for /");
    assert!(
        matches!(req, TaskReq::ExplorerList { path, .. } if path == "/"),
        "Backspace climbs to the root"
    );
}

fn core_seed_host(_w: &mut ExplorerWindow, rx: &mut mpsc::UnboundedReceiver<TaskReq>) {
    while rx.try_recv().is_ok() {}
}

/// Final-review fix (M2): the `..` parent pseudo-row is not renamable —
/// `targets()`/Del already filter it, and renaming it would target the
/// parent directory.
#[test]
fn rename_ignores_the_parent_row() {
    let (mut w, _core, mut rx) = window();
    while rx.try_recv().is_ok() {}
    w.host.path = "/home/u".into();
    seed(
        &mut w,
        ExplorerTarget::Host,
        "/home/u",
        vec![entry("a.txt", false)],
    );
    // `seed` syncs with one Down; `g` parks the cursor on row 0 (`..`).
    send(&mut w, Key::Char('g'));
    w.pending_prompt = Some(PromptKind::Rename);
    *w.prompt_value.borrow_mut() = "x".into();
    w.modal_answer = Some(tv::Command::OK);
    w.finish_prompt();
    assert!(rx.try_recv().is_err(), "the `..` row is refused");

    // On a real row the rename is still queued.
    send(&mut w, Key::Down);
    w.pending_prompt = Some(PromptKind::Rename);
    *w.prompt_value.borrow_mut() = "b.txt".into();
    w.modal_answer = Some(tv::Command::OK);
    w.finish_prompt();
    let req = rx.try_recv().expect("rename queued");
    assert!(
        matches!(req, TaskReq::ExplorerRename { ref old, ref new, .. }
                if old == "/home/u/a.txt" && new == "/home/u/b.txt"),
        "rename targets the cursor row's name"
    );
}

#[test]
fn copy_direction_follows_the_focused_panel() {
    // Review Focus #4/#2: `X` copies AWAY from the focused panel.
    let (mut w, _core, mut rx) = window();
    while rx.try_recv().is_ok() {}
    // The host panel opens at $HOME; pin it so the seeded listing
    // matches the panel's path and the copy target is deterministic.
    w.host.path = "/home/u".into();
    seed(
        &mut w,
        ExplorerTarget::Host,
        "/home/u",
        vec![entry("a.txt", false)],
    );
    seed(
        &mut w,
        ExplorerTarget::Container("c1".into()),
        "/",
        vec![entry("b.txt", false)],
    );

    // Host focused → copy TO the container.
    send(&mut w, Key::Char('X'));
    let req = rx.try_recv().expect("copy queued");
    assert!(
        matches!(req, TaskReq::ExplorerCopy { to_container: true, ref items, ref dst, .. }
                if items == &vec!["/home/u/a.txt".to_string()] && dst == "/"),
        "host→container copy targets the host's selected-or-cursor file"
    );

    // Remote focused → copy TO the host.
    send(&mut w, Key::Tab);
    send(&mut w, Key::Char('X'));
    let req = rx.try_recv().expect("copy queued");
    assert!(
        matches!(req, TaskReq::ExplorerCopy { to_container: false, ref items, ref dst, .. }
                if items == &vec!["/b.txt".to_string()] && dst == "/home/u"),
        "container→host copy targets the remote's file into the host panel's dir"
    );
}

#[test]
fn del_targets_the_focused_panel_and_no_sends_nothing() {
    // Review Focus #2.
    let (mut w, _core, mut rx) = window();
    while rx.try_recv().is_ok() {}
    w.host.path = "/home/u".into();
    seed(
        &mut w,
        ExplorerTarget::Host,
        "/home/u",
        vec![entry("a.txt", false), entry("b.txt", false)],
    );

    // Space-select two rows, then Del → the confirm is window-owned:
    // it must queue NOTHING until the box is answered YES. The box
    // runs under run_program in the integration test; here we drive
    // set_modal_answer directly (the ResourceWindow picker pattern).
    send(&mut w, Key::Char(' '));
    send(&mut w, Key::Down);
    send(&mut w, Key::Char(' '));
    send(&mut w, Key::Delete);
    assert!(rx.try_recv().is_err(), "no delete before the answer");
    // Simulate the box answering NO.
    {
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        let mut out = std::collections::VecDeque::new();
        let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
        w.set_modal_answer(tv::Command::NO);
        let mut ev = Event::Command(CMD_EXP_DELETE_CONFIRMED);
        w.handle_event(&mut ev, &mut ctx);
    }
    assert!(rx.try_recv().is_err(), "NO ⇒ nothing queued");
    // And YES queues exactly the two selected host paths.
    send(&mut w, Key::Delete);
    {
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        let mut out = std::collections::VecDeque::new();
        let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
        w.set_modal_answer(tv::Command::YES);
        let mut ev = Event::Command(CMD_EXP_DELETE_CONFIRMED);
        w.handle_event(&mut ev, &mut ctx);
    }
    let req = rx.try_recv().expect("delete queued");
    assert!(
        matches!(req, TaskReq::ExplorerDelete { ref paths, .. }
                if paths == &vec!["/home/u/a.txt".to_string(), "/home/u/b.txt".to_string()]),
        "exactly the selection"
    );
    assert!(rx.try_recv().is_err(), "exactly one");
}

#[test]
fn del_ignores_the_parent_row() {
    let (mut w, _core, mut rx) = window();
    while rx.try_recv().is_ok() {}
    // Remote at /app (show_parent true); cursor on "..".
    w.remote.path = "/app".into();
    w.remote.show_parent = true;
    seed(
        &mut w,
        ExplorerTarget::Container("c1".into()),
        "/app",
        vec![entry("x", false)],
    );
    // The remote panel is the one showing the `..` row.
    send(&mut w, Key::Tab);
    send(&mut w, Key::Delete);
    {
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        let mut out = std::collections::VecDeque::new();
        let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
        w.set_modal_answer(tv::Command::YES);
        let mut ev = Event::Command(CMD_EXP_DELETE_CONFIRMED);
        w.handle_event(&mut ev, &mut ctx);
    }
    assert!(
        rx.try_recv().is_err(),
        "the `..` row is never a delete target"
    );
}

#[test]
fn drop_stops_the_volume_helper_but_never_a_container_explorer() {
    // Review Focus #3: every volume-window close must stop its
    // helper; a container explorer must never send the request.
    let core = Rc::new(RefCell::new(AppCore::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<TaskReq>();
    let open = Rc::new(RefCell::new(false));
    drop(build(
        Rc::clone(&core),
        Rect::new(0, 0, 76, 23),
        "vol:data".into(),
        ExplorerTarget::Volume("data".into()),
        "data".into(),
        Rc::clone(&open),
        tx,
    ));
    let mut saw_list = false;
    let mut saw_stop = false;
    while let Ok(req) = rx.try_recv() {
        match req {
            TaskReq::RemoveVolumeHelper { ref name } => {
                assert_eq!(name, "data");
                saw_stop = true;
            }
            TaskReq::ExplorerList { .. } => saw_list = true,
            _ => {}
        }
    }
    assert!(saw_list && saw_stop, "list requested and helper stopped");
    assert!(!*open.borrow(), "Drop cleared the open flag");

    let (tx, mut rx) = mpsc::unbounded_channel::<TaskReq>();
    drop(build(
        core,
        Rect::new(0, 0, 76, 23),
        "c1".into(),
        ExplorerTarget::Container("c1".into()),
        "web".into(),
        Rc::new(RefCell::new(false)),
        tx,
    ));
    while let Ok(req) = rx.try_recv() {
        assert!(
            !matches!(req, TaskReq::RemoveVolumeHelper { .. }),
            "a container explorer never stops a helper"
        );
    }
}

/// Drive a listing into the window as if the worker had answered it.
fn seed(w: &mut ExplorerWindow, target: ExplorerTarget, path: &str, entries: Vec<ExplorerEntry>) {
    let key = w.key.clone();
    let side_target = target;
    w.core.borrow_mut().apply(crate::core::AppMsg::ExplorerDir {
        key,
        target: side_target,
        path: path.to_string(),
        entries,
    });
    crate::testutil::send_key(w, Key::Down); // one event → sync
}

/// The breadcrumb and the hint must sit INSIDE the frame, not on it
/// (bug report: both rows overlapped the window border — the hint was
/// even truncated by the bottom-right corner).
#[test]
fn the_breadcrumb_and_hint_sit_inside_the_frame_not_on_it() {
    let (mut w, _core, _rx) = window();
    crate::testutil::with_ctx(|ctx| w.redraw_texts(ctx));
    let host_path = ExplorerWindow::host_home();
    let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
    let lines: Vec<&str> = out.lines().collect();
    let n = lines.len();
    assert!(n >= 4, "the window rendered: {out:?}");

    // Row 0 is the window's TOP frame: it carries the title, never text.
    assert!(
        lines[0].contains("Explorer") && !lines[0].contains(&host_path),
        "the breadcrumb is not drawn on the top border: {:?}",
        lines[0]
    );
    // …and the breadcrumb (the host's path) is the first CLIENT row.
    assert!(
        lines[1].contains(&host_path),
        "the breadcrumb is the first client row: {:?}",
        lines[1]
    );
    // The hint is the LAST CLIENT row, one above the bottom border.
    assert!(
        lines[n - 2].contains("Tab panels"),
        "the hint sits inside the frame: {:?}",
        lines[n - 2]
    );
    // The bottom border stays a border — and no longer truncates the hint.
    assert!(
        lines[n - 1].ends_with('┘'),
        "the bottom border stays a border: {:?}",
        lines[n - 1]
    );
    assert!(
        !lines[n - 1].contains("Backspace"),
        "the hint is not clipped by the corner: {:?}",
        lines[n - 1]
    );
}

#[test]
fn resize_refills_every_child_with_the_new_window_size() {
    use crate::testutil::render_view;
    let (mut w, _core, _rx) = window();
    // First draw records the build-time size…
    let _ = render_view(80, 25, &mut w);
    // …then the window changes bounds (zoom / Size-Move / terminal resize).
    w.change_bounds(Rect::new(2, 1, 62, 18));
    let _ = render_view(80, 25, &mut w);

    let size = w.state().size;
    let half = size.x / 2;
    let bottom = size.y - 1; // last CLIENT row index (the frame owns the rest)
    let cases = [
        (
            "breadcrumb (host)",
            w.breadcrumb_host_id,
            Rect::new(1, 1, half - 1, 2),
        ),
        (
            "breadcrumb (container)",
            w.breadcrumb_remote_id,
            Rect::new(half + 1, 1, size.x - 1, 2),
        ),
        ("host filter", w.host.filter, Rect::new(1, 2, half - 1, 3)),
        (
            "remote filter",
            w.remote.filter,
            Rect::new(half + 1, 2, size.x - 1, 3),
        ),
        (
            "host table",
            w.host.table,
            Rect::new(1, 3, half - 1, bottom - 1),
        ),
        (
            "remote table",
            w.remote.table,
            Rect::new(half + 1, 3, size.x - 1, bottom - 1),
        ),
        (
            "hint",
            w.hint_id,
            Rect::new(1, bottom - 1, size.x - 1, bottom),
        ),
    ];
    for (name, id, want) in cases {
        let got = w
            .win
            .child_mut(id)
            .expect("child present")
            .state()
            .get_bounds();
        assert_eq!(got, want, "{name} must refill the resized window");
    }
}
/// A left click activates the panel it lands on: the other side must
/// yield (the bug report: clicking the inactive panel did nothing).
#[test]
fn a_click_activates_the_panel_it_lands_on() {
    let (mut w, _core, _rx) = window();
    seed(&mut w, ExplorerTarget::Host, "/", vec![entry("h1", false)]);
    seed(
        &mut w,
        ExplorerTarget::Container("c1".into()),
        "/",
        vec![entry("r1", false)],
    );
    let mid = 76 / 2;
    let cursor_x = |w: &mut ExplorerWindow| {
        crate::testutil::text(&crate::testutil::render_view(76, 23, w))
            .lines()
            .find_map(|l| l.find('>'))
    };
    assert!(!w.active_remote, "the host side is active at open");

    click(&mut w, mid + 4, 4, 0);
    assert!(w.active_remote, "clicking the other panel activates it");
    let x = cursor_x(&mut w).expect("a cursor is drawn");
    assert!(
        x >= mid as usize,
        "the cursor moved to the clicked panel: x={x}"
    );

    click(&mut w, 4, 4, 0);
    assert!(!w.active_remote, "clicking back returns to the host");
}

/// Two clicks on the SAME row within the interval open it (the
/// file-manager convention — the same request Enter makes).
#[test]
fn a_second_click_on_the_same_row_opens_it() {
    let (mut w, _core, mut rx) = window();
    core_seed_host(&mut w, &mut rx); // drain the two build requests
    // The REMOTE panel is rooted at "/" and has no ".." row, so its only
    // entry is data row 0.
    seed(
        &mut w,
        ExplorerTarget::Container("c1".into()),
        "/",
        vec![entry("app", true)],
    );
    while rx.try_recv().is_ok() {}
    let mid = 76 / 2;
    let x = mid + 4;
    let y = 4; // the header owns local y=3, so data row 0 is y=4

    click(&mut w, x, y, 1_000);
    assert!(rx.try_recv().is_err(), "one click only activates");

    click(&mut w, x, y, 1_200);
    let req = rx.try_recv().expect("the second click opens the row");
    assert!(
        matches!(req, TaskReq::ExplorerList { path, .. } if path == "/app"),
        "a second click opens the directory"
    );
    assert!(w.active_remote, "the clicked panel became active");
}

/// …but a click on the same row LATER than the interval is just another
/// single click — it must not open anything.
#[test]
fn a_click_beyond_the_interval_only_activates() {
    let (mut w, _core, mut rx) = window();
    core_seed_host(&mut w, &mut rx);
    seed(
        &mut w,
        ExplorerTarget::Container("c1".into()),
        "/",
        vec![entry("app", true)],
    );
    while rx.try_recv().is_ok() {}
    let x = 76 / 2 + 4;

    click(&mut w, x, 4, 10_000);
    click(&mut w, x, 4, 11_000); // 1 s later: a fresh first click
    assert!(
        rx.try_recv().is_err(),
        "a click outside the interval must not open"
    );
}

/// Only the panel that receives the keys draws its cursor row (bug
/// report: both the host and the container side looked active, so there
/// was no way to tell where a keypress would go).
#[test]
fn only_the_active_panel_draws_the_cursor_marker() {
    let (mut w, _core, _rx) = window();
    // Give both sides something to draw a cursor on.
    seed(&mut w, ExplorerTarget::Host, "/", vec![entry("h1", false)]);
    seed(
        &mut w,
        ExplorerTarget::Container("c1".into()),
        "/",
        vec![entry("r1", false)],
    );
    let mid = 76 / 2;
    let cursor_x = |w: &mut ExplorerWindow| {
        crate::testutil::text(&crate::testutil::render_view(76, 23, w))
            .lines()
            .find_map(|l| l.find('>'))
    };

    // The host side is active at open.
    let x = cursor_x(&mut w).expect("a cursor is drawn");
    assert!(x < mid, "the host side is active: x={x}");

    // Tab hands the keys to the container side: its cursor lights up.
    send(&mut w, Key::Tab);
    let x = cursor_x(&mut w).expect("a cursor is drawn");
    assert!(x >= mid, "the container side is active: x={x}");

    // …and back again.
    send(&mut w, Key::Tab);
    let x = cursor_x(&mut w).expect("a cursor is drawn");
    assert!(x < mid, "the host side is active again: x={x}");
}
/// The two text rows must be READABLE (contrast) and the breadcrumb
/// must show each side's absolute path above ITS column.
///
/// Bug report: both rows were near-invisible (`Role::Disabled` is
/// `85,85,85` on blue — a 1.8:1 ratio, far under WCAG AA's 4.5:1), and
/// the breadcrumb jammed both paths into one string that started at the
/// left edge, out of alignment with the panes below it.
#[test]
fn the_breadcrumb_is_readable_and_aligned_to_its_column() {
    let (mut w, _core, _rx) = window();
    crate::testutil::with_ctx(|ctx| w.redraw_texts(ctx));
    let screen = crate::testutil::render_view(76, 23, &mut w);
    let buf = screen.buffer();
    let r = explorer_rects(w.state().size);
    let host_path = ExplorerWindow::host_home();

    /// WCAG contrast ratio of one cell against its own background.
    fn contrast(buf: &tv::Buffer, x: i32, y: i32) -> f64 {
        let lum = |r: f64, g: f64, b: f64| {
            let f = |v: f64| {
                let v = v / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
        };
        let rgb = |c: tv::Color| match c {
            tv::Color::Rgb(r, g, b) => (r as f64, g as f64, b as f64),
            _ => (255.0, 255.0, 255.0),
        };
        let st = buf.get(x as u16, y as u16).style();
        let (fr, fgc, fb) = rgb(st.fg);
        let (br, bgc, bb) = rgb(st.bg);
        let (a, b) = (lum(fr, fgc, fb), lum(br, bgc, bb));
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    let cell = |rect: Rect, y: i32| -> String {
        (rect.a.x..rect.b.x)
            .map(|x| buf.get(x as u16, y as u16).symbol().to_string())
            .collect()
    };
    let host_y = r.breadcrumb_host.a.y;
    let host_cell = cell(r.breadcrumb_host, host_y);
    let remote_cell = cell(r.breadcrumb_remote, host_y);
    assert!(
        host_cell.contains(&host_path),
        "the host path sits in the HOST column: {host_cell:?}"
    );
    assert!(
        remote_cell.contains(&w.remote.path),
        "the container path sits in the CONTAINER column: {remote_cell:?}"
    );

    // Contrast: WCAG AA needs 4.5:1 for normal text.
    let probe = |rect: Rect, y: i32| (rect.a.x + 1, y);
    let (x, y) = probe(r.breadcrumb_host, host_y);
    assert!(
        contrast(&buf, x, y) >= 4.5,
        "the breadcrumb is readable: ratio={:.2}",
        contrast(&buf, x, y)
    );
    let hint_y = r.hint.a.y;
    assert!(
        contrast(&buf, r.hint.a.x + 1, hint_y) >= 4.5,
        "the hint is readable: ratio={:.2}",
        contrast(&buf, r.hint.a.x + 1, hint_y)
    );
}
/// A window whose remote side is an image .
fn image_window() -> (ExplorerWindow, mpsc::UnboundedReceiver<TaskReq>) {
    let core = Rc::new(RefCell::new(AppCore::new()));
    let (tx, rx) = mpsc::unbounded_channel::<TaskReq>();
    let w = build(
        core,
        Rect::new(0, 0, 76, 23),
        "img:sha256:abc".into(),
        ExplorerTarget::Image("sha256:abc".into()),
        "alpine:latest".into(),
        Rc::new(RefCell::new(true)),
        tx,
    );
    (w, rx)
}

/// The image side's menu offers only `copy to host` + `refresh`; the host
/// side offers only `refresh` (nothing may write into the image, and the
/// host pane is a copy destination, not a workbench).
#[test]
fn the_image_context_menu_is_read_only() {
    let (mut w, _rx) = image_window();
    w.active_remote = true;
    let on_image = w
        .context_menu_items()
        .into_iter()
        .map(|(l, _)| l.replace('~', ""))
        .collect::<Vec<_>>();
    assert!(on_image.iter().any(|l| l == "copy to host"), "{on_image:?}");
    assert!(on_image.iter().any(|l| l == "Refresh"), "{on_image:?}");
    assert!(
        !on_image.iter().any(|l| l.contains("Delete")),
        "{on_image:?}"
    );
    assert!(
        !on_image.iter().any(|l| l.contains("Rename")),
        "{on_image:?}"
    );
    assert!(!on_image.iter().any(|l| l.contains("ew ")), "{on_image:?}");

    w.active_remote = false;
    let on_host = w
        .context_menu_items()
        .into_iter()
        .map(|(l, _)| l.replace('~', ""))
        .collect::<Vec<_>>();
    assert_eq!(on_host, vec!["Refresh".to_string()], "{on_host:?}");
}

/// `X` copies only from the IMAGE side (image → host); from the host side
/// it must not fire, because that direction would write into the image.
#[test]
fn copy_fires_only_from_the_image_side() {
    use crate::domain::docker::explorer::ExplorerEntry;
    let dir = ExplorerEntry {
        name: "etc".into(),
        is_dir: true,
        size: 0,
        modified: "Oct 11 09:00".into(),
        permissions: "drwxr-xr-x".into(),
    };
    let (mut w, mut rx) = image_window();
    while rx.try_recv().is_ok() {}
    seed(
        &mut w,
        ExplorerTarget::Image("sha256:abc".into()),
        "/",
        vec![dir.clone()],
    );
    seed(
        &mut w,
        ExplorerTarget::Host,
        &ExplorerWindow::host_home(),
        vec![dir],
    );

    // Focus the host table and put the cursor on a REAL row: `targets()`
    // drops the `..` parent, so without this `copy_active` bails on
    // "nothing selected" BEFORE any direction logic — which let this test
    // pass with both direction gates removed (review mutations M5c-M5e).
    let host_table = w.host.table;
    crate::testutil::with_ctx(|ctx| {
        w.win.state_mut().state.focused = true;
        w.win.focus_descendant(host_table, ctx);
    });
    send(&mut w, Key::Down); // off the `..` parent row

    w.active_remote = false; // the HOST side is active
    send(&mut w, Key::Char('X'));
    assert!(rx.try_recv().is_err(), "host → image is refused");

    w.active_remote = true; // the IMAGE side is active
    send(&mut w, Key::Char('X'));
    assert!(
        matches!(
            rx.try_recv(),
            Ok(TaskReq::ExplorerCopy {
                to_container: false,
                ..
            })
        ),
        "image → host is allowed, and is the only direction"
    );
}

/// `Del`, `n`, `N` and `r` do nothing on an image target.
///
/// Asserted on the state each key would have changed, NOT on the task
/// channel: `Del` opens a message box and `n`/`N`/`r` post `CMD_EXP_PROMPT`
/// to the ctx queue, so a channel-only assertion could never fail — it
/// would pass with the read-only gate removed entirely.
#[test]
fn del_and_new_and_rename_are_inert_on_an_image_target() {
    use crate::domain::docker::explorer::ExplorerEntry;
    let (mut w, mut rx) = image_window();
    while rx.try_recv().is_ok() {}
    // A row, so `targets()` is non-empty and the arms would actually fire.
    seed(
        &mut w,
        ExplorerTarget::Image("sha256:abc".into()),
        "/",
        vec![ExplorerEntry {
            name: "etc".into(),
            is_dir: true,
            size: 0,
            modified: "Oct 11 09:00".into(),
            permissions: "drwxr-xr-x".into(),
        }],
    );
    w.active_remote = true;

    for key in [Key::Delete, Key::Char('n'), Key::Char('N'), Key::Char('r')] {
        let mut out = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        let mut ctx = Context::new(&mut out, &mut timers, 0, &mut deferred);
        let mut ev = tv::event::Event::KeyDown(tv::event::KeyEvent::new(
            key,
            tv::event::KeyModifiers::default(),
        ));
        w.handle_event(&mut ev, &mut ctx);
        assert!(out.is_empty(), "key {key:?} posted nothing: {out:?}");
        assert!(w.pending_prompt.is_none(), "no prompt for {key:?}");
        assert!(w.pending_delete.is_none(), "no delete for {key:?}");
    }
    assert!(rx.try_recv().is_err(), "nothing reaches the worker");
}

/// Closing the window must stop the helper.
#[test]
fn drop_sends_remove_image_helper() {
    let (w, mut rx) = image_window();
    while rx.try_recv().is_ok() {} // drain the two build requests
    drop(w);
    assert!(
        matches!(
            rx.try_recv(),
            Ok(TaskReq::RemoveImageHelper { image }) if image == "sha256:abc"
        ),
        "the window reaps its helper"
    );
}
