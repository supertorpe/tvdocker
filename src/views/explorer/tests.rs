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

/// Focus the window group and descend to the host table — the
/// desktop program does this before children can report `focused`. The
/// `settle_currency` pass first establishes each table group's own currency
/// (its body), which the pump would otherwise do between events; without it
/// keys would land on the table group and never reach the row body.
fn focus_panel(w: &mut ExplorerWindow, remote: bool) {
    let id = w.panel_table(remote);
    crate::testutil::with_ctx(|ctx| {
        w.win.state_mut().state.focused = true;
        w.win.settle_currency(ctx);
        w.win.focus_descendant(id, ctx);
    });
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
    // The group.rs pattern standalone tests must recreate: mark the group
    // focused, settle its currency, then descend to the host table.
    focus_panel(&mut w, false);
    (w, core, rx)
}

fn send(w: &mut ExplorerWindow, key: Key) {
    crate::testutil::send_key(w, key);
}

/// Deliver a left `MouseDown` at window-local `(x, y)`. `double` sets the
/// framework double-click flag the pump synthesizes (`m.flags.double_click`).
fn mouse(w: &mut ExplorerWindow, x: i32, y: i32, double: bool) {
    crate::testutil::with_ctx(|ctx| {
        let mut ev = Event::MouseDown(tv::MouseEvent {
            position: tv::Point::new(x, y),
            buttons: tv::event::MouseButtons {
                left: true,
                ..Default::default()
            },
            flags: tv::event::MouseEventFlags {
                double_click: double,
                ..Default::default()
            },
            ..tv::MouseEvent::default()
        });
        w.handle_event(&mut ev, ctx);
    });
}

/// The table row ids of panel `remote`, in display order.
fn table_ids(w: &mut ExplorerWindow, remote: bool) -> Vec<String> {
    let id = w.panel_table(remote);
    let Some(t) = w.table_mut(id) else {
        return Vec::new();
    };
    let order = t.row_order();
    let model = t.model();
    order.iter().map(|&row| model.row_key(row).0).collect()
}

/// The focused row's key in panel `remote`, if any.
fn focused_key(w: &mut ExplorerWindow, remote: bool) -> Option<String> {
    let id = w.panel_table(remote);
    w.table_mut(id).and_then(|t| t.focused_key()).map(|k| k.0)
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
    assert_eq!(
        table_ids(&mut w, true),
        vec!["app".to_string(), "notes.txt".to_string()],
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
    // The upstream `Table` is a `Group`, so the focus flag on the table
    // child IS meaningful — the window's Tab interception is exactly
    // what must keep `focused` on a table and never on a filter bar.
    let (mut w, _core, _rx) = window();
    let host_table = w.host.table;
    let remote_table = w.remote.table;
    let host_filter = w.host.filter;
    let remote_filter = w.remote.filter;
    let focused =
        |w: &mut ExplorerWindow, id| w.win.child_mut(id).is_some_and(|v| v.state().state.focused);
    assert!(!w.active_remote(), "host side active first");
    assert!(
        focused(&mut w, host_table) && !focused(&mut w, remote_table),
        "opens on the host table"
    );
    assert!(
        !focused(&mut w, host_filter) && !focused(&mut w, remote_filter),
        "opens on a table, not a filter"
    );

    // Tab 1: host → remote.
    send(&mut w, Key::Tab);
    assert!(w.active_remote(), "Tab → remote table");
    assert!(
        focused(&mut w, remote_table) && !focused(&mut w, host_table),
        "Tab hands the focus to the remote table"
    );
    assert!(
        !focused(&mut w, host_filter) && !focused(&mut w, remote_filter),
        "Tab never stops on a filter"
    );
    // Tab 2: exactly two stops, wrapping back to the host table.
    send(&mut w, Key::Tab);
    assert!(!w.active_remote(), "Tab wraps to the host table");
    assert!(
        focused(&mut w, host_table) && !focused(&mut w, remote_table),
        "still exactly two stops"
    );

    // `/` focuses the active (host) filter; typing narrows; Esc
    // returns to the host table.
    send(&mut w, Key::Char('/'));
    assert!(focused(&mut w, host_filter), "/ → the host filter");
    send(&mut w, Key::Char('a'));
    let typed = w
        .win
        .child_mut(host_filter)
        .and_then(|v| v.as_any_mut())
        .and_then(|a| a.downcast_mut::<FilterBar>())
        .map(|f| f.value().to_string());
    assert_eq!(typed.as_deref(), Some("a"), "typing narrows the filter");
    send(&mut w, Key::Esc);
    assert!(
        focused(&mut w, host_table) && !focused(&mut w, host_filter),
        "Esc returns focus to the table"
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
    w.prompt_value = "x".into();
    w.modal_answer = Some(tv::Command::OK);
    w.finish_prompt();
    assert!(rx.try_recv().is_err(), "the `..` row is refused");

    // On a real row the rename is still queued.
    send(&mut w, Key::Down);
    w.pending_prompt = Some(PromptKind::Rename);
    w.prompt_value = "b.txt".into();
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
    assert!(!w.active_remote(), "the host side is active at open");

    mouse(&mut w, mid + 4, 4, false);
    assert!(w.active_remote(), "clicking the other panel activates it");
    assert_eq!(
        focused_key(&mut w, true).as_deref(),
        Some("r1"),
        "the click moved the cursor to the clicked panel's row"
    );

    mouse(&mut w, 4, 4, false);
    assert!(!w.active_remote(), "clicking back returns to the host");
}

/// A click carrying the framework's double-click flag opens the row under
/// it (the file-manager convention — the same request Enter makes).
#[test]
fn a_double_click_opens_the_row_it_lands_on() {
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
    let x = 76 / 2 + 4;
    let y = 4; // the header owns local y=0, so data row 0 is y=4

    mouse(&mut w, x, y, false);
    assert!(rx.try_recv().is_err(), "one click only activates");

    mouse(&mut w, x, y, true);
    let req = rx.try_recv().expect("the double-click opens the row");
    assert!(
        matches!(req, TaskReq::ExplorerList { path, .. } if path == "/app"),
        "a double-click opens the directory"
    );
    assert!(w.active_remote(), "the clicked panel became active");
}

/// Only the panel that receives the keys holds the focus (bug report: both
/// the host and the container side looked active, so there was no way to tell
/// where a keypress would go). The inactive panel now dims automatically
/// (the upstream body draws `Role::ListInactive`); the flag asserted here is
/// what selects the dimming and the key routing.
#[test]
fn only_the_focused_panel_holds_the_focus() {
    let (mut w, _core, _rx) = window();
    // Give both sides something to draw a cursor on.
    seed(&mut w, ExplorerTarget::Host, "/", vec![entry("h1", false)]);
    seed(
        &mut w,
        ExplorerTarget::Container("c1".into()),
        "/",
        vec![entry("r1", false)],
    );
    let host_table = w.host.table;
    let remote_table = w.remote.table;

    // The host side is focused at open.
    assert!(
        w.is_focused(host_table) && !w.is_focused(remote_table),
        "the host side is focused"
    );

    // Tab hands the keys to the container side.
    send(&mut w, Key::Tab);
    assert!(
        w.is_focused(remote_table) && !w.is_focused(host_table),
        "the container side is focused"
    );

    // …and back again.
    send(&mut w, Key::Tab);
    assert!(
        w.is_focused(host_table) && !w.is_focused(remote_table),
        "the host side is focused again"
    );
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
    focus_panel(&mut w, true);
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

    focus_panel(&mut w, false);
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
    focus_panel(&mut w, false);
    send(&mut w, Key::Down); // off the `..` parent row

    send(&mut w, Key::Char('X'));
    assert!(rx.try_recv().is_err(), "host → image is refused");

    focus_panel(&mut w, true); // the IMAGE side is active
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
    focus_panel(&mut w, true);

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

/// Windows ▸ Full screen reaches ExplorerWindow's inner window: enter Desktop
/// frameless, one press reverts with the frame and bounds back.
#[test]
fn full_screen_toggles_the_explorer_window() {
    use tv::Fullscreen;
    let (mut w, _core, _rx) = window();
    let orig = w.win.state().get_bounds();
    let mut consumed = false;
    crate::testutil::with_ctx(|ctx| {
        ctx.set_owner_size(tv::Point::new(80, 25));
        let mut ev = Event::Command(crate::app::CMD_FULLSCREEN);
        w.handle_event(&mut ev, ctx);
        consumed = ev.is_nothing();
    });
    assert!(consumed, "ExplorerWindow consumes CMD_FULLSCREEN");
    assert_eq!(w.win.fullscreen(), Fullscreen::Desktop, "enters Desktop");
    assert!(!w.win.bordered(), "frameless");
    assert_eq!(
        w.win.state().get_bounds(),
        Rect::new(0, 0, 80, 25),
        "fills the owner"
    );

    crate::testutil::with_ctx(|ctx| {
        ctx.set_owner_size(tv::Point::new(80, 25));
        let mut ev = Event::Command(crate::app::CMD_FULLSCREEN);
        w.handle_event(&mut ev, ctx);
    });
    assert_eq!(w.win.fullscreen(), Fullscreen::Off, "one press reverts");
    assert!(w.win.bordered(), "frame back");
    assert_eq!(w.win.state().get_bounds(), orig, "bounds restored");
}
