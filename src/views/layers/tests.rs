use super::*;
use crate::domain::layers::{Analysis, DiffType, Entry, Kind, Layer, Tree, hash_bytes};
use crate::testutil;
use tv::Role;
use tv::event::{Key, KeyEvent, KeyModifiers};

fn mods(ctrl: bool) -> KeyModifiers {
    KeyModifiers {
        ctrl,
        ..KeyModifiers::default()
    }
}

fn entry(path: &str, contents: &str) -> Entry {
    Entry {
        path: path.to_string(),
        size: contents.len() as u64,
        mode: 0o644,
        uid: 0,
        gid: 0,
        kind: Kind::File,
        link: None,
        hash: hash_bytes(contents.as_bytes()),
    }
}

fn analysis() -> Analysis {
    let mut t0 = Tree::new();
    t0.add_path(entry("/bin/busybox", "v1"));
    let mut t1 = Tree::new();
    t1.add_path(entry("/etc/hosts", "127.0.0.1"));
    Analysis {
        layers: vec![
            Layer {
                index: 0,
                id: "sha256:aaa".into(),
                command: "FROM alpine".into(),
                size: 2_600_000,
                removed: Vec::new(),
                tree: t0,
            },
            Layer {
                index: 1,
                id: "sha256:bbb".into(),
                command: "RUN apk add".into(),
                size: 1_200_000,
                removed: Vec::new(),
                tree: t1,
            },
        ],
        efficiency: 1.0,
        wasted_bytes: 0,
        inefficient: Vec::new(),
    }
}

/// Layer 0 sets `/etc/hosts` + `/etc/passwd`; layer 1 changes hosts and adds
/// a new file; layer 2 leaves passwd untouched.
fn three_layer_analysis() -> Analysis {
    let mut t0 = Tree::new();
    t0.add_path(entry("/etc/hosts", "v1"));
    t0.add_path(entry("/etc/passwd", "v1"));
    let mut t1 = Tree::new();
    t1.add_path(entry("/etc/hosts", "v2")); // modified
    t1.add_path(entry("/etc/new", "v1")); // added
    let mut t2 = Tree::new();
    t2.add_path(entry("/etc/passwd", "v1")); // survives unchanged
    Analysis {
        layers: vec![
            Layer {
                index: 0,
                id: "a".into(),
                command: "FROM".into(),
                size: 100,
                removed: Vec::new(),
                tree: t0,
            },
            Layer {
                index: 1,
                id: "b".into(),
                command: "RUN".into(),
                size: 200,
                removed: Vec::new(),
                tree: t1,
            },
            Layer {
                index: 2,
                id: "c".into(),
                command: "COPY".into(),
                size: 300,
                removed: Vec::new(),
                tree: t2,
            },
        ],
        efficiency: 1.0,
        wasted_bytes: 0,
        inefficient: Vec::new(),
    }
}

/// Sync from the core AND push into the tables — a test-side shim for what
/// `handle_event` does with a real `Context`.
fn sync_and_draw(win: &mut LayersWindow) {
    win.sync_from_core();
    crate::testutil::with_ctx(|ctx| win.redraw(ctx));
}

/// A window over a seeded core, synced and drawn — the shared setup for
/// the key/filter tests.
fn window_with(a: Analysis) -> LayersWindow {
    let mut core = crate::core::AppCore::new();
    core.image_layers.insert("sha256:aaa".into(), Some(a));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut win = build(
        Rc::new(RefCell::new(core)),
        Rect::new(0, 0, 80, 24),
        "sha256:aaa".into(),
        "img".into(),
        Rc::new(RefCell::new(false)),
        tx,
    );
    sync_and_draw(&mut win);
    win
}

#[test]
fn the_change_tree_carries_a_role_per_change_type() {
    assert_eq!(role_for(DiffType::Added), Some(Role::Success));
    assert_eq!(role_for(DiffType::Removed), Some(Role::Error));
    assert_eq!(role_for(DiffType::Modified), Some(Role::Warning));
    assert_eq!(role_for(DiffType::Unmodified), None);
}

#[test]
fn the_second_layer_marks_an_add_and_a_modification() {
    let mut core = crate::core::AppCore::new();
    core.image_layers
        .insert("sha256:aaa".into(), Some(three_layer_analysis()));
    let core = Rc::new(RefCell::new(core));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut win = build(
        core,
        Rect::new(0, 0, 80, 24),
        "sha256:aaa".into(),
        "img".into(),
        Rc::new(RefCell::new(false)),
        tx,
    );
    win.selected = 1;
    sync_and_draw(&mut win);
    let texts: Vec<&str> = win.tree_rows.iter().map(|r| r.path.as_str()).collect();
    assert!(texts.contains(&"/etc/new"), "{texts:?}");
    let added = win.tree_rows.iter().find(|r| r.path == "/etc/new").unwrap();
    assert_eq!(added.diff, DiffType::Added);
    let modified = win
        .tree_rows
        .iter()
        .find(|r| r.path == "/etc/hosts")
        .unwrap();
    assert_eq!(modified.diff, DiffType::Modified);
    // The tint reaches the table model (design §7): Added rows take the
    // Success role, Modified rows Warning, Unmodified rows none.
    let table = win.table_mut(win.tree_table).expect("tree table");
    let model = table.model();
    let role_of = |path: &str| {
        let row = (0..model.row_count())
            .find(|i| model.row_key(*i).0 == path)
            .unwrap_or_else(|| panic!("{path} row present"));
        model.row_role(row)
    };
    assert_eq!(
        role_of("/etc/new"),
        Some(Role::Success),
        "/etc/new is Added"
    );
    assert_eq!(
        role_of("/etc/hosts"),
        Some(Role::Warning),
        "/etc/hosts is Modified"
    );
    assert_eq!(role_of("/etc/passwd"), None, "/etc/passwd is Unmodified");
}

#[test]
fn the_inefficient_strip_lists_paths_ascending() {
    let mut a = three_layer_analysis();
    a.inefficient = vec![
        crate::domain::layers::InefficientPath {
            count: 2,
            wasted: 10,
            path: "/small".into(),
        },
        crate::domain::layers::InefficientPath {
            count: 3,
            wasted: 900,
            path: "/big".into(),
        },
    ];
    let mut core = crate::core::AppCore::new();
    core.image_layers.insert("sha256:aaa".into(), Some(a));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut win = build(
        Rc::new(RefCell::new(core)),
        Rect::new(0, 0, 80, 24),
        "sha256:aaa".into(),
        "img".into(),
        Rc::new(RefCell::new(false)),
        tx,
    );
    sync_and_draw(&mut win);
    let text = testutil::text(&testutil::render_view(80, 24, &mut win));
    assert!(text.contains("/small"), "{text}");
    assert!(text.contains("/big"), "{text}");
}

#[test]
fn baseline_rows_carry_size_and_permissions_from_the_comparison_tree() {
    // layer 0 writes /etc/hosts; layer 1 touches only /etc/new.
    let mut t0 = Tree::new();
    t0.add_path(entry("/etc/hosts", "v1"));
    let mut t1 = Tree::new();
    t1.add_path(entry("/etc/new", "v1"));
    let a = Analysis {
        layers: vec![
            Layer {
                index: 0,
                id: "a".into(),
                command: "FROM".into(),
                size: 100,
                removed: Vec::new(),
                tree: t0,
            },
            Layer {
                index: 1,
                id: "b".into(),
                command: "RUN".into(),
                size: 200,
                removed: Vec::new(),
                tree: t1,
            },
        ],
        efficiency: 1.0,
        wasted_bytes: 0,
        inefficient: Vec::new(),
    };
    let mut core = crate::core::AppCore::new();
    core.image_layers.insert("sha256:aaa".into(), Some(a));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut win = build(
        Rc::new(RefCell::new(core)),
        Rect::new(0, 0, 80, 24),
        "sha256:aaa".into(),
        "img".into(),
        Rc::new(RefCell::new(false)),
        tx,
    );
    win.selected = 1;
    sync_and_draw(&mut win);
    // `/etc/hosts` is Unmodified and only exists in the COMPARISON tree
    // (layer 1's own tar never wrote it) — its row still needs size+mode.
    let table = win.table_mut(win.tree_table).expect("tree table");
    let model = table.model();
    let row = (0..model.row_count())
        .find(|i| model.row_key(*i).0 == "/etc/hosts")
        .expect("/etc/hosts is in the comparison tree");
    assert_eq!(&*model.cell(row, 1), " ", "Unmodified letter");
    assert_eq!(&*model.cell(row, 2), "2B", "size from the comparison tree");
    assert_eq!(
        &*model.cell(row, 3),
        "-rw-r--r--",
        "permissions as an rwx string (dive's Permission column); the pane is too \
         narrow for a UID:GID column, so the PERM cell follows SIZE"
    );
}

#[test]
fn an_empty_layers_analysis_clears_the_panes_without_panicking() {
    let mut core = crate::core::AppCore::new();
    core.image_layers
        .insert("sha256:aaa".into(), Some(Analysis::default()));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut win = build(
        Rc::new(RefCell::new(core)),
        Rect::new(0, 0, 80, 24),
        "sha256:aaa".into(),
        "img".into(),
        Rc::new(RefCell::new(false)),
        tx,
    );
    // Must not reach `Comparer::tree`, whose `stack(&layers[0..=0])`
    // slices an empty vector; the clear branch runs instead.
    sync_and_draw(&mut win);
    assert!(win.tree_rows.is_empty());
    let table = win.table_mut(win.tree_table).expect("tree table");
    assert_eq!(table.model().row_count(), 0, "the pane stays cleared");
}

#[test]
fn child_rows_are_indented_one_step_deeper_than_their_parent() {
    let mut t = Tree::new();
    t.add_path(entry("/hosts", "v1")); // top level
    t.add_path(entry("/etc/hosts", "v1")); // one level down
    t.add_path(entry("/etc/sub/x", "v1")); // two levels down
    let rows = tree_rows_for(&flatten(&t), &t, &HashSet::new(), &tree_columns(60));
    // One step = one 4-cell indent block (`    ` / `│   `), dive's
    // `renderStringTreeBetween`; the connector follows the blocks.
    let leading = |path: &str| {
        let row = rows.iter().find(|r| r.id == path).expect(path);
        row.cells[0].len() - row.cells[0].trim_start().len()
    };
    assert_eq!(leading("/hosts"), 0, "top-level file");
    assert_eq!(leading("/etc"), 0, "top-level directory");
    assert_eq!(leading("/etc/hosts"), 4, "one step deeper than /etc");
    assert_eq!(leading("/etc/sub"), 4, "level-1 directory");
    assert_eq!(leading("/etc/sub/x"), 8, "two steps deeper than /etc");
}

#[test]
fn the_window_titles_itself_and_lists_every_layer() {
    let core = Rc::new(RefCell::new(crate::core::AppCore::new()));
    core.borrow_mut()
        .image_layers
        .insert("sha256:aaa".into(), Some(analysis()));
    let open = Rc::new(RefCell::new(false));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut win = build(
        core,
        Rect::new(0, 0, 80, 24),
        "sha256:aaa".into(),
        "alpine:3.19".into(),
        open,
        tx,
    );
    sync_and_draw(&mut win);
    let text = testutil::text(&testutil::render_view(80, 24, &mut win));
    assert!(text.contains("Layers — alpine:3.19"), "{text}");
    assert!(text.contains("FROM alpine"), "{text}");
    assert!(text.contains("RUN apk add"), "{text}");
    assert!(text.contains("Efficiency"), "{text}");
}

/// Ctrl+T's observable (spec §10's acceptance): the window title cannot
/// change at runtime in tvision-rs 0.17 (verified exhaustively), so the
/// compare mode is rendered as a prefix on the always-visible summary
/// line. The old name of this test claimed it checked "the title" while
/// it only ever asserted the `mode` field.
#[test]
fn ctrl_t_toggles_the_compare_mode_and_the_summary_indicator() {
    let mut win = window_with(three_layer_analysis());
    assert_eq!(win.mode, Compare::Single);
    assert!(
        win.summary.borrow().starts_with(" Current layer · "),
        "Single mode is announced on row 1: {:?}",
        *win.summary.borrow()
    );
    testutil::send_event(
        &mut win,
        Event::KeyDown(KeyEvent::new(Key::Char('t'), mods(true))),
    );
    assert_eq!(win.mode, Compare::Aggregated, "Ctrl+T flips the mode");
    assert!(
        win.summary.borrow().starts_with(" Aggregated · "),
        "…and so is Aggregated mode: {:?}",
        *win.summary.borrow()
    );
    // The indicator reaches the SCREEN — row 1 is always visible.
    let text = testutil::text(&testutil::render_view(80, 24, &mut win));
    assert!(text.contains("Aggregated · L0 a · Total"), "{text}");
    testutil::send_event(
        &mut win,
        Event::KeyDown(KeyEvent::new(Key::Char('t'), mods(true))),
    );
    assert_eq!(win.mode, Compare::Single);
    assert!(
        win.summary.borrow().starts_with(" Current layer · "),
        "back to Single mode: {:?}",
        *win.summary.borrow()
    );
}

#[test]
fn plus_and_minus_expand_and_collapse_the_focused_directory() {
    let mut win = window_with(three_layer_analysis());
    win.selected = 1;
    sync_and_draw(&mut win);
    // `/etc` starts expanded (the root always is).
    assert!(win.visible_rows().iter().any(|r| r.path == "/etc/hosts"));
    win.collapse("/etc");
    assert!(!win.visible_rows().iter().any(|r| r.path == "/etc/hosts"));
    win.expand("/etc");
    assert!(win.visible_rows().iter().any(|r| r.path == "/etc/hosts"));
}

/// Collapsing a DEEP directory hides every descendant of it — not just
/// one level down (the ancestor check walks full path prefixes).
#[test]
fn collapsing_a_nested_directory_hides_its_whole_subtree() {
    let mut t0 = Tree::new();
    t0.add_path(entry("/etc/sub/deep/x", "v1"));
    let a = Analysis {
        layers: vec![Layer {
            index: 0,
            id: "a".into(),
            command: "FROM".into(),
            size: 100,
            removed: Vec::new(),
            tree: t0,
        }],
        efficiency: 1.0,
        wasted_bytes: 0,
        inefficient: Vec::new(),
    };
    let mut win = window_with(a);
    sync_and_draw(&mut win);
    assert!(
        win.visible_rows()
            .iter()
            .any(|r| r.path == "/etc/sub/deep/x")
    );
    win.collapse("/etc/sub");
    let still: Vec<String> = win.visible_rows().into_iter().map(|r| r.path).collect();
    assert!(
        !still.iter().any(|p| p == "/etc/sub/deep/x"),
        "the subtree is hidden: {still:?}"
    );
    assert!(
        !still.iter().any(|p| p == "/etc/sub/deep"),
        "the child directory too: {still:?}"
    );
    assert!(
        still.iter().any(|p| p == "/etc/sub"),
        "the collapsed directory itself stays: {still:?}"
    );
    assert!(
        still.iter().any(|p| p == "/etc"),
        "its ancestors stay: {still:?}"
    );
}

/// Finding 3 (spec §7): after `-` collapses a directory, the row must
/// SHOW it is collapsed — dive's `⊕ ` marker (`file_tree.go`,
/// `collapsedItem`) in the PATH cell; expanding removes it again.
#[test]
fn collapsing_a_directory_marks_its_row_with_the_collapse_glyph() {
    let mut win = window_with(three_layer_analysis());
    win.selected = 1;
    sync_and_draw(&mut win);
    let path_cell = |win: &mut LayersWindow, path: &str| -> String {
        let table = win.table_mut(win.tree_table).expect("tree table");
        let model = table.model();
        let row = (0..model.row_count())
            .find(|i| model.row_key(*i).0 == path)
            .unwrap_or_else(|| panic!("{path} row present"));
        model.cell(row, 0).to_string()
    };
    assert_eq!(path_cell(&mut win, "/etc"), "└─ etc", "expanded: no marker");
    win.collapse("/etc");
    sync_and_draw(&mut win);
    assert_eq!(
        path_cell(&mut win, "/etc"),
        "└─ ⊕ etc",
        "collapsed: marker behind the connector"
    );
    win.expand("/etc");
    sync_and_draw(&mut win);
    assert_eq!(path_cell(&mut win, "/etc"), "└─ etc", "expanded again");
}

#[test]
fn the_a_key_hides_added_rows() {
    let mut win = window_with(three_layer_analysis());
    win.selected = 1;
    sync_and_draw(&mut win);
    assert!(win.visible_rows().iter().any(|r| r.path == "/etc/new"));
    win.toggle_diff(DiffType::Added);
    assert!(!win.visible_rows().iter().any(|r| r.path == "/etc/new"));
    win.toggle_diff(DiffType::Added);
    assert!(win.visible_rows().iter().any(|r| r.path == "/etc/new"));
}

/// Finding 1's gate observable: a `LayerProgress` tick must not touch a
/// CACHED analysis (the `Rc` pointer must survive — a re-read would hand
/// out a fresh allocation) and must not schedule a rebuild (`dirty` stays
/// clear, so `handle_event` skips `redraw`). Key the sync gate on
/// `layers_rev` again (or drop it) and the deep re-clone replaces the
/// `Rc` → the `Rc::ptr_eq` below fails; drop the dirty gate and the
/// `assert!(!win.dirty)` fails on the very next tick.
#[test]
fn a_progress_tick_neither_reclones_nor_schedules_a_rebuild_for_a_cached_window() {
    let mut win = window_with(three_layer_analysis());
    assert!(!win.dirty, "the first draw consumed the build-time dirt");
    let cached = win.analysis.clone().expect("the analysis is cached");
    // One tick of the `docker save` progress stream, bridged between two
    // events.
    win.core
        .borrow_mut()
        .apply(crate::core::AppMsg::LayerProgress {
            key: "sha256:aaa".into(),
            bytes: 4096,
        });
    win.sync_from_core(); // what every event runs first
    assert!(
        win.analysis
            .as_ref()
            .is_some_and(|a| Rc::ptr_eq(a, &cached)),
        "a progress tick must not deep-clone the analysis"
    );
    assert!(!win.dirty, "…and must not schedule a rebuild");
    {
        let core = win.core.borrow();
        assert_eq!(core.layers_rev, 1, "the progress rev moved");
        assert_eq!(
            core.layers_data_rev, 0,
            "…the data rev did not (the window's gate)"
        );
    }
    // A finished analysis IS a data change: fresh `Rc`, rebuild scheduled.
    win.core
        .borrow_mut()
        .apply(crate::core::AppMsg::ImageLayers {
            key: "sha256:aaa".into(),
            analysis: Box::new(three_layer_analysis()),
        });
    win.sync_from_core();
    assert!(win.dirty, "the data rev moved → the models rebuild");
    assert!(
        !win.analysis
            .as_ref()
            .is_some_and(|a| Rc::ptr_eq(a, &cached)),
        "a finished analysis is a fresh wrap"
    );
}

/// The other half of the dirty spec: while the slot is `None`, a NEW byte
/// count marks the window dirty so the "Analyzing …" line updates (the
/// line itself is written by the gated `redraw`).
#[test]
fn a_progress_tick_marks_a_loading_window_dirty_so_the_analyzing_line_updates() {
    let core = crate::core::AppCore::new();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut win = build(
        Rc::new(RefCell::new(core)),
        Rect::new(0, 0, 80, 24),
        "sha256:aaa".into(),
        "img".into(),
        Rc::new(RefCell::new(false)),
        tx,
    );
    sync_and_draw(&mut win);
    assert!(!win.dirty, "the first draw consumed the build-time dirt");
    win.core
        .borrow_mut()
        .apply(crate::core::AppMsg::LayerProgress {
            key: "sha256:aaa".into(),
            bytes: 4096,
        });
    win.sync_from_core();
    assert!(
        win.dirty,
        "a loading window re-renders the Analyzing line per tick"
    );
    crate::testutil::with_ctx(|ctx| win.redraw(ctx));
    let summary = win.summary.borrow();
    assert!(
        summary.contains(&format!(
            "Analyzing {}",
            crate::views::resource::format_size(4096)
        )),
        "the new byte count reached the line: {summary}"
    );
}

/// The primary interaction: the layer table's cursor OWNS `selected`, so
/// arrowing to another layer rebuilds the comparison tree for it in the
/// SAME event. (Task 9's tests assigned `selected` directly, which is
/// exactly how the missing wiring stayed hidden.)
#[test]
fn selecting_a_different_layer_rebuilds_the_tree_for_that_layer() {
    let mut win = window_with(three_layer_analysis());
    // `selected` starts on layer 0: a layer compared against itself marks
    // nothing — no added rows.
    assert_eq!(win.selected, 0);
    assert!(!win.tree_rows.iter().any(|r| r.path == "/etc/new"));
    // Focus the layer table the way the pump does (the explorer test's
    // pattern: mark the group focused, settle its currency, descend),
    // then arrow down onto row 1.
    crate::testutil::with_ctx(|ctx| {
        win.win.state_mut().state.focused = true;
        win.win.settle_currency(ctx);
        win.win.focus_descendant(win.layer_table, ctx);
    });
    testutil::send_key(&mut win, Key::Down);
    // ONE event: the cursor moved → `selected` followed → the tree
    // rebuilt for layer 1's changes.
    assert_eq!(win.selected, 1, "the layer cursor drives `selected`");
    let diffs: Vec<(&str, DiffType)> = win
        .tree_rows
        .iter()
        .map(|r| (r.path.as_str(), r.diff))
        .collect();
    assert!(
        diffs.contains(&("/etc/new", DiffType::Added)),
        "layer 1's tree: {diffs:?}"
    );
    assert!(
        diffs.contains(&("/etc/hosts", DiffType::Modified)),
        "layer 1's tree: {diffs:?}"
    );
    // The `▏` marker followed (redraw re-rendered the layer rows).
    {
        let table = win.table_mut(win.layer_table).expect("layer table");
        let model = table.model();
        assert_eq!(&*model.cell(1, 0), "▏", "marker on layer 1");
        assert_eq!(&*model.cell(0, 0), " ", "…and off layer 0");
    }
    // It keeps moving: a second Down lands on layer 2 — which also proves
    // `set_model` kept the cursor on its RowKey across the rebuild (a
    // reset cursor would move 0→1 and never reach 2).
    testutil::send_key(&mut win, Key::Down);
    assert_eq!(win.selected, 2, "the cursor was not fought back to 0");
}

/// The path filter bar must actually narrow the tree (spec §7): typed
/// through the real key path, one event per keystroke — no second event
/// may be needed for the sink the `FilterBar` just wrote to reach the
/// model.
#[test]
fn the_path_filter_bar_narrows_the_tree_as_you_type() {
    let mut win = window_with(three_layer_analysis());
    win.selected = 1;
    // Park the layer table's cursor on the SAME row: `handle_event` syncs
    // `selected` FROM that cursor, so a bare `selected = 1` would be
    // snapped back to row 0 by the first keystroke.
    crate::testutil::with_ctx(|ctx| {
        let t = win.table_mut(win.layer_table).expect("layer table");
        t.set_focused_key(&tv::widgets::RowKey("L1".to_string()), ctx);
    });
    sync_and_draw(&mut win);
    // The pump does this before children can report `focused` (the
    // explorer test's pattern): mark the group focused, settle its
    // currency, then descend to the tree table.
    crate::testutil::with_ctx(|ctx| {
        win.win.state_mut().state.focused = true;
        win.win.settle_currency(ctx);
        win.win.focus_descendant(win.tree_table, ctx);
    });
    // `/` sends focus to the active pane's filter bar — the tree table
    // holds the focus, so it's the path filter.
    testutil::send_key(&mut win, Key::Char('/'));
    assert!(win.is_focused(win.tree_filter), "the path bar holds focus");
    for c in "new".chars() {
        testutil::send_key(&mut win, Key::Char(c));
    }
    assert_eq!(&*win.tree_filter_sink.borrow(), "new");
    let keys: Vec<String> = {
        let table = win.table_mut(win.tree_table).expect("tree table");
        let model = table.model();
        (0..model.row_count()).map(|i| model.row_key(i).0).collect()
    };
    assert_eq!(
        keys,
        vec!["/etc/new".to_string()],
        "one event per keystroke: only the needle's path survives"
    );
    // Clearing the sink restores the whole tree.
    win.tree_filter_sink.borrow_mut().clear();
    sync_and_draw(&mut win);
    assert!(win.visible_rows().iter().any(|r| r.path == "/etc/hosts"));
}

/// A child's current bounds (the bars' placement assertions).
fn child_bounds(win: &mut LayersWindow, id: tv::ViewId) -> Rect {
    win.win.child_mut(id).unwrap().state().get_bounds()
}

/// `redraw` with a Context that KEEPS its deferred queue, so a test can
/// read the scroll-bar parameter requests the tables publish to their bars.
fn redraw_capturing_deferred(win: &mut LayersWindow) -> Vec<tv::Deferred> {
    let mut out = std::collections::VecDeque::new();
    let mut timers = tv::TimerQueue::new();
    let mut deferred: Vec<tv::Deferred> = Vec::new();
    {
        let mut ctx = tv::Context::new(&mut out, &mut timers, 0, &mut deferred);
        win.redraw(&mut ctx);
    }
    deferred
}

/// The two tables give their right-most client column to a vertical scroll
/// bar (the `widgets::scroll_rects` body+bar convention): the bar is one
/// column wide and starts exactly where its table's body ends.
#[test]
fn layer_and_tree_tables_each_reserve_a_scroll_bar_column() {
    let r = layers_rects(tv::Point::new(80, 24));
    assert_eq!(r.layer_table, Rect::new(1, 3, 30, 20));
    assert_eq!(r.layer_bar, Rect::new(30, 3, 31, 20));
    assert_eq!(r.tree_table, Rect::new(33, 3, 78, 20));
    assert_eq!(r.tree_bar, Rect::new(78, 3, 79, 20));
}

/// Degenerate widths must never produce an inverted bar rect.
#[test]
fn the_scroll_bar_rects_stay_ordered_at_degenerate_sizes() {
    for w in 1..8 {
        let r = layers_rects(tv::Point::new(w, 4));
        for bar in [r.layer_bar, r.tree_bar] {
            assert!(bar.a.x <= bar.b.x && bar.a.y <= bar.b.y, "w={w}: {bar:?}");
        }
    }
}

/// The window inserts one `ScrollBar` per table at that table's right edge,
/// and the bars stay out of the Tab order.
#[test]
fn both_tables_get_a_vertical_scroll_bar_that_tab_skips() {
    let mut win = window_with(analysis());
    let (layer_bar, tree_bar) = (win.layer_bar, win.tree_bar);
    let r = layers_rects(win.win.state().size);
    assert_eq!(child_bounds(&mut win, layer_bar), r.layer_bar);
    assert_eq!(child_bounds(&mut win, tree_bar), r.tree_bar);
    for id in [layer_bar, tree_bar] {
        assert!(
            !win.win.child_mut(id).unwrap().state().options.selectable,
            "the bar participates in neither Tab order nor focus"
        );
    }
}

/// The id plumbing, end to end: on redraw each table publishes its row
/// range to ITS bar (the pump applies these deferred params to the
/// `ScrollBar` views), so a scrollbar click can drive that table.
#[test]
fn each_table_publishes_its_row_range_to_its_bar() {
    let mut win = window_with(three_layer_analysis());
    win.selected = 1;
    sync_and_draw(&mut win);
    let deferred = redraw_capturing_deferred(&mut win);
    let bar_max = |id: tv::ViewId| {
        deferred.iter().find_map(|d| match d {
            tv::Deferred::ScrollBarSetParams { id: got, max, .. } if *got == id => *max,
            _ => None,
        })
    };
    assert_eq!(
        bar_max(win.layer_bar),
        Some(2),
        "3 layers ⇒ the layer bar's max is len-1"
    );
    let tree_rows = win.tree_rows.len() as i32;
    assert_eq!(bar_max(win.tree_bar), Some(tree_rows - 1));
}

/// Zoom / resize re-derives the bars' rects with the tables'.
#[test]
fn the_bars_follow_the_tables_through_a_resize() {
    let mut win = window_with(analysis());
    win.change_bounds(Rect::new(0, 0, 60, 20));
    let (layer_bar, tree_bar) = (win.layer_bar, win.tree_bar);
    let r = layers_rects(win.win.state().size);
    assert_eq!(child_bounds(&mut win, layer_bar), r.layer_bar);
    assert_eq!(child_bounds(&mut win, tree_bar), r.tree_bar);
}

/// The bars are VISIBLE: each table's right-most column renders the
/// scroll-bar glyphs (up arrow on the header row, thumb in the body,
/// down arrow at the foot).
#[test]
fn the_scroll_bars_render_in_the_tables_right_most_columns() {
    let mut win = window_with(three_layer_analysis());
    let screen = testutil::render_view(80, 24, &mut win);
    let text = testutil::text(&screen);
    let lines: Vec<&str> = text.lines().collect();
    let row_containing = |needle: &str| {
        lines
            .iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("a row contains {needle:?}"))
    };
    let cell = |line: &str, x: usize| line.chars().nth(x);
    // The bar column of each table (window coords == screen coords here).
    let header = row_containing("CMP");
    let body = row_containing("FROM");
    let foot = row_containing("▼");
    for (x, name) in [(30, "layer"), (78, "tree")] {
        assert_eq!(cell(header, x), Some('▲'), "{name} bar's up arrow");
        assert_eq!(cell(body, x), Some('▓'), "{name} bar's thumb");
        assert_eq!(cell(foot, x), Some('▼'), "{name} bar's down arrow");
    }
}

/// The PATH column renders the tree the way dive does: `├─`/`└─`
/// connectors, `│` continuations for ancestors with more siblings below,
/// a `→ target` suffix for symlinks — plus the rwx permission string and
/// the uid:gid of the entry.
#[test]
fn tree_rows_render_connectors_symlink_targets_permissions_and_ids() {
    let mut t = Tree::new();
    t.add_path(entry("/bin/busybox", "v1"));
    let mut link = entry("/bin/sh", "");
    link.kind = Kind::Symlink;
    link.link = Some("/bin/busybox".into());
    t.add_path(link);
    t.add_path(entry("/etc/hosts", "v1"));
    let rows = flatten(&t);
    let rendered = tree_rows_for(&rows, &t, &HashSet::new(), &tree_columns(60));
    let cell = |path: &str, col: usize| {
        rendered
            .iter()
            .find(|r| r.id == path)
            .unwrap_or_else(|| panic!("{path} is rendered"))
            .cells[col]
            .clone()
    };
    assert_eq!(cell("/bin", 0), "\u{251c}\u{2500} bin");
    assert_eq!(
        cell("/bin/busybox", 0),
        "\u{2502}   \u{251c}\u{2500} busybox"
    );
    assert_eq!(
        cell("/bin/sh", 0),
        "\u{2502}   \u{2514}\u{2500} sh \u{2192} /bin/busybox"
    );
    assert_eq!(cell("/etc", 0), "\u{2514}\u{2500} etc");
    assert_eq!(cell("/etc/hosts", 0), "    \u{2514}\u{2500} hosts");
    assert_eq!(cell("/bin", 2), "2B", "the dir rolls its subtree up");
    assert_eq!(cell("/etc", 2), "2B", "…wherever its files sit");
    assert_eq!(cell("/bin/busybox", 2), "2B", "a file keeps its own size");
    assert_eq!(cell("/bin/busybox", 4), "-rw-r--r--", "the mode as rwx");
    assert_eq!(cell("/bin/busybox", 3), "0:0", "the uid:gid");
    assert_eq!(cell("/bin", 4), "", "a synthesised dir carries no mode");
}

/// The attribute columns only appear when the pane is wide enough for
/// them (dive drops the attributes in a constrained layout): PATH keeps
/// the leftover, and each attribute column claims a fixed slice of it.
#[test]
fn the_tree_columns_adapt_to_the_pane_width() {
    let labels = |width: i32| -> Vec<String> {
        tree_columns(width)
            .iter()
            .map(|c| c.label.clone())
            .collect()
    };
    assert_eq!(
        labels(60),
        vec!["PATH", "CHANGE", "SIZE", "UID:GID", "PERM"]
    );
    assert_eq!(labels(42), vec!["PATH", "CHANGE", "SIZE", "PERM"]);
    assert_eq!(labels(30), vec!["PATH", "CHANGE", "SIZE"]);
    // The PATH column keeps whatever the fixed columns leave over.
    assert!(tree_columns(60)[0].width >= 16, "PATH stays readable");
}

/// The layer pane is the narrow one now, so its COMMAND column takes the
/// leftover width instead of a fixed 28 cells.
#[test]
fn the_layer_command_column_takes_the_leftover_width() {
    let cols = layer_columns(30);
    assert_eq!(
        cols.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
        vec!["CMP", "SIZE", "COMMAND"]
    );
    assert_eq!(cols[2].width, 19, "30 - CMP(3) - SIZE(8)");
}

/// The summary line names the selected layer (its diff id, the way dive's
/// Layer Details pane does) and keeps one decimal of the efficiency.
#[test]
fn the_summary_line_names_the_selected_layer_and_keeps_a_decimal() {
    let mut a = analysis();
    a.efficiency = 0.9959688;
    let mut win = window_with(a);
    win.selected = 1;
    sync_and_draw(&mut win);
    let summary = win.summary.borrow().clone();
    assert!(
        summary.contains("L1 sha256:bbb"),
        "the layer id: {summary:?}"
    );
    assert!(
        summary.contains("Efficiency 99.6%"),
        "no round-up to 100: {summary:?}"
    );
}
