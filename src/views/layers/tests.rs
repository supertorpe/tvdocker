use super::*;
use crate::domain::layers::{Analysis, DiffType, Entry, Kind, Layer, Tree, hash_bytes};
use crate::testutil;
use tv::Role;

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
        (0..model.row_count())
            .find(|i| model.row_key(*i).0 == path)
            .and_then(|i| model.row_role(i))
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
