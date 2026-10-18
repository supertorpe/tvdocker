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
fn baseline_rows_carry_size_and_mode_from_the_comparison_tree() {
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
    assert_eq!(&*model.cell(row, 3), "644", "mode from the comparison tree");
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
    let rows = tree_rows_for(&flatten(&t), &t);
    let leading = |path: &str| {
        let row = rows.iter().find(|r| r.id == path).expect(path);
        row.cells[0].len() - row.cells[0].trim_start().len()
    };
    assert_eq!(leading("/hosts"), 0, "top-level file");
    assert_eq!(leading("/etc"), 0, "top-level directory");
    assert_eq!(leading("/etc/hosts"), 1, "one step deeper than /etc");
    assert_eq!(leading("/etc/sub"), 1, "level-1 directory");
    assert_eq!(leading("/etc/sub/x"), 2, "two steps deeper than /etc");
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

#[test]
fn ctrl_t_toggles_the_compare_mode_and_the_title() {
    let mut win = window_with(three_layer_analysis());
    assert_eq!(win.mode, Compare::Single);
    testutil::send_event(
        &mut win,
        Event::KeyDown(KeyEvent::new(Key::Char('t'), mods(true))),
    );
    assert_eq!(win.mode, Compare::Aggregated, "Ctrl+T flips the mode");
    testutil::send_event(
        &mut win,
        Event::KeyDown(KeyEvent::new(Key::Char('t'), mods(true))),
    );
    assert_eq!(win.mode, Compare::Single);
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
