//! The Layers window, end to end: seeded core → the Images window's
//! two-hop → the window renders the layers, drives the window keys through
//! the real event loop, and asks the worker once.
#![cfg(test)]

mod common;

use common::{app_start, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key, KeyModifiers};
use tvdocker::core::{AppCore, PendingKind, TaskReq};
use tvdocker::domain::layers::{Analysis, Entry, Kind, Layer, Tree, hash_bytes};

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

/// One layer: a history command, its tree, and the whiteouts it records.
fn layer(index: usize, command: &str, size: u64, tree: Tree, removed: &[&str]) -> Layer {
    Layer {
        index,
        id: format!("sha256:{index}"),
        command: command.into(),
        size,
        removed: removed.iter().map(|p| (*p).to_string()).collect(),
        tree,
    }
}

fn analysis_of(layers: Vec<Layer>) -> Analysis {
    Analysis {
        layers,
        efficiency: 1.0,
        wasted_bytes: 0,
        inefficient: Vec::new(),
    }
}

/// A tree holding the single file `path`.
fn tree_of(path: &str) -> Tree {
    let mut t = Tree::new();
    t.add_path(entry(path, "v1"));
    t
}

fn analysis() -> Analysis {
    analysis_of(vec![
        layer(0, "FROM alpine", 2_600_000, tree_of("/bin/busybox"), &[]),
        layer(1, "RUN apk add", 1_200_000, tree_of("/etc/hosts"), &[]),
    ])
}

/// A real tombstone: layer 0 writes `/etc/hosts` and `/etc/passwd`; layer 1
/// whiteouts hosts and re-lists passwd untouched. At layer 1 (Single) the
/// comparison must render hosts as Removed with its `Entry` back-filled.
fn whiteout_analysis() -> Analysis {
    let mut t0 = Tree::new();
    t0.add_path(entry("/etc/hosts", "127.0.0.1"));
    t0.add_path(entry("/etc/passwd", "root:x"));
    let mut t1 = Tree::new();
    t1.add_path(entry("/etc/passwd", "root:x"));
    analysis_of(vec![
        layer(0, "FROM alpine", 2_600_000, t0, &[]),
        layer(1, "RUN apk del", 1_200_000, t1, &["/etc/hosts"]),
    ])
}

/// `layer0={/a}, layer1={/b}, layer2={/c}` — the two compare modes differ
/// exactly where the key test needs them: at layer 2, Single marks `/b`
/// Unmodified (it is in the baseline) while Aggregated marks it Added (it is
/// new since layer 0). At layer 0 both keys collapse to `(0,0,0,0)`, so the
/// cursor must be moved down first.
fn three_layer_analysis() -> Analysis {
    analysis_of(vec![
        layer(0, "FROM alpine", 1_000_000, tree_of("/a"), &[]),
        layer(1, "RUN apk add b", 1_000_000, tree_of("/b"), &[]),
        layer(2, "RUN apk add c", 1_000_000, tree_of("/c"), &[]),
    ])
}

/// The image row plus the seeded analysis, so the opener finds a cursor row
/// and the window finds renderable data (the harness seeds BEFORE
/// `app::build`, so this precedes any window existing).
fn seed(core: &Rc<RefCell<AppCore>>, a: Analysis) {
    core.borrow_mut()
        .images
        .push(tvdocker::domain::docker::images::ImageEntry {
            id: "sha256:aaa".into(),
            repository: "alpine".into(),
            tag: "3.19".into(),
            size: 3_800_000,
        });
    core.borrow_mut()
        .image_layers
        .insert("sha256:aaa".into(), Some(a));
}

/// The two-hop the Images window performs: seed the pending cursor row, then
/// the desktop's opener command.
fn open_layers(h: &mut common::AppHarness) {
    h.core
        .borrow_mut()
        .set_pending(vec!["sha256:aaa".into()], PendingKind::Images);
    h.screen
        .push_event(Event::Command(tvdocker::app::CMD_LAYERS));
    pump(&mut h.program, 10);
}

fn ctrl() -> KeyModifiers {
    KeyModifiers {
        ctrl: true,
        ..KeyModifiers::default()
    }
}

/// The one rendered line containing `needle` — the row locator the letter
/// assertions build on. Fixture strings are unique per pane, so the line
/// found is the tree row being asserted about.
fn line_with<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line renders {needle:?}:\n{text}"))
}

#[test]
fn the_layers_window_renders_the_layers_and_asks_the_worker_once() {
    let mut h = app_start(|core| seed(core, analysis()));
    // The two-hop: mint the cursor row, then the desktop's opener.
    open_layers(&mut h);
    let text = screen_text(&h.screen);
    // The title is the proof that `open_layers` itself ran — Task 11's tests
    // stopped at the posted command and at `transform`, before the opener.
    assert!(text.contains("Layers — alpine:3.19"), "{text}");
    assert!(text.contains("FROM alpine"), "{text}");
    assert!(text.contains("RUN apk add"), "{text}");
    // The window asks the worker exactly once, keyed by the image id.
    let mut requests = 0;
    while let Ok(req) = h.task_rx.try_recv() {
        if let TaskReq::AnalyzeImageLayers { key, image } = req {
            assert_eq!(key, "sha256:aaa");
            assert_eq!(image, "sha256:aaa");
            requests += 1;
        }
    }
    assert_eq!(requests, 1, "one analysis request per open");
}

#[test]
fn invoking_the_opener_twice_never_duplicates_the_window() {
    let mut h = app_start(|core| seed(core, analysis()));
    for _ in 0..2 {
        open_layers(&mut h);
    }
    let text = screen_text(&h.screen);
    let windows = text.matches("Layers —").count();
    assert_eq!(windows, 1, "reopen focuses, it never duplicates: {text}");
}

/// The whole whiteout chain — tombstone (`Layer::removed`) → `stack` →
/// `compare` → the rendered row — proven on screen. `tree_rows_for` renders
/// the change letter in the CHANGE cell (`Removed='-'`, `Unmodified=' '`),
/// and no fixture or chrome string on the located line contains `-`, so
/// `contains('-')` is exactly "this row's letter is `-`".
#[test]
fn a_whiteout_renders_as_a_deletion_on_the_removed_paths_row() {
    let mut h = app_start(|core| seed(core, whiteout_analysis()));
    open_layers(&mut h);
    // Layer 0 against itself marks nothing: the tombstone must not show yet.
    let text = screen_text(&h.screen);
    assert!(
        !line_with(&text, "hosts").contains('-'),
        "layer 0 wrote hosts and nothing removed it yet: {text}"
    );
    // One Down: the cursor reaches layer 1 and the tree rebuilds for it.
    h.screen.push_key(Key::Down, KeyModifiers::default());
    pump(&mut h.program, 2);
    let text = screen_text(&h.screen);
    assert!(
        line_with(&text, "hosts").contains('-'),
        "the whiteout renders as a deletion: {text}"
    );
    assert!(
        !line_with(&text, "passwd").contains('-'),
        "the untouched survivor is not marked deleted: {text}"
    );
}

/// Task 10's window keys, driven as real keystrokes through the pump — the
/// unit tests called `toggle_mode`/`toggle_diff` directly, so deleting a
/// `window_keys` arm would not have failed anything. Single and Aggregated
/// agree on layer 0, so the cursor walks down to layer 2 first (which also
/// exercises `focused_layer`'s cursor→`selected` wiring at program level).
#[test]
fn ctrl_t_and_a_reshape_the_tree_through_the_real_key_path() {
    let mut h = app_start(|core| seed(core, three_layer_analysis()));
    open_layers(&mut h);
    // The layer table holds the initial focus; two Downs land on layer 2.
    for _ in 0..2 {
        h.screen.push_key(Key::Down, KeyModifiers::default());
        pump(&mut h.program, 2);
    }
    // Single at layer 2: the baseline (/a, /b) is Unmodified, only /c is Added.
    let text = screen_text(&h.screen);
    assert!(
        !line_with(&text, "RUN apk add b").contains('+'),
        "Single: /b sits in the baseline, so no Added letter: {text}"
    );
    assert!(
        line_with(&text, "RUN apk add c").contains('+'),
        "Single: /c was just added by layer 2: {text}"
    );
    // Ctrl+T → Aggregated (layer 0 vs the whole stack): /b becomes Added.
    h.screen.push_key(Key::Char('t'), ctrl());
    pump(&mut h.program, 2);
    let text = screen_text(&h.screen);
    assert!(
        line_with(&text, "RUN apk add b").contains('+'),
        "Aggregated: /b is new since layer 0: {text}"
    );
    // `a` hides the Added rows: /b and /c leave the tree, Unmodified /a stays
    // (every rendered tree row carries mode 644; the layer table has no mode
    // column, so the count is the rendered tree-row count).
    h.screen.push_key(Key::Char('a'), KeyModifiers::default());
    pump(&mut h.program, 2);
    let text = screen_text(&h.screen);
    assert!(
        !line_with(&text, "RUN apk add b").contains('+'),
        "the Added filter hid /b: {text}"
    );
    assert_eq!(
        text.matches("644").count(),
        1,
        "only the Unmodified /a row survives the filter: {text}"
    );
    // Both keys toggle back — a one-way toggle must fail here.
    h.screen.push_key(Key::Char('a'), KeyModifiers::default());
    pump(&mut h.program, 2);
    let text = screen_text(&h.screen);
    assert_eq!(
        text.matches("644").count(),
        3,
        "the second `a` restores the Added rows: {text}"
    );
    h.screen.push_key(Key::Char('t'), ctrl());
    pump(&mut h.program, 2);
    let text = screen_text(&h.screen);
    assert!(
        !line_with(&text, "RUN apk add b").contains('+'),
        "the second Ctrl+T returns to Single: {text}"
    );
}
