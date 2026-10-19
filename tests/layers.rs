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

/// The image row only — no cached analysis, so the window must ask the
/// worker for one (finding 1.4's UNCACHED path).
fn seed_image(core: &Rc<RefCell<AppCore>>) {
    core.borrow_mut()
        .images
        .push(tvdocker::domain::docker::images::ImageEntry {
            id: "sha256:aaa".into(),
            repository: "alpine".into(),
            tag: "3.19".into(),
            size: 3_800_000,
        });
}

/// The image row plus the seeded analysis, so the opener finds a cursor row
/// and the window finds renderable data (the harness seeds BEFORE
/// `app::build`, so this precedes any window existing).
fn seed(core: &Rc<RefCell<AppCore>>, a: Analysis) {
    seed_image(core);
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

/// Drain the task channel, counting `AnalyzeImageLayers` requests (each
/// one's key/image is checked while at it).
fn count_layer_requests(h: &mut common::AppHarness) -> usize {
    let mut requests = 0;
    while let Ok(req) = h.task_rx.try_recv() {
        if let TaskReq::AnalyzeImageLayers { key, image } = req {
            assert_eq!(key, "sha256:aaa");
            assert_eq!(image, "sha256:aaa");
            requests += 1;
        }
    }
    requests
}

/// Finding 1.4: image ids are content-addressed (spec D9), so the cached
/// analysis can never be stale — a reopen renders it and never restarts
/// the `docker save` stream. (The pre-final-fix version of this test
/// seeded the cache AND expected a request; sub-item 4 makes those two
/// assertions mutually exclusive, so the request half moved to
/// [`an_uncached_image_asks_the_worker_exactly_once`].)
#[test]
fn the_layers_window_renders_a_cached_analysis_without_reasking_the_worker() {
    let mut h = app_start(|core| seed(core, analysis()));
    // The two-hop: mint the cursor row, then the desktop's opener.
    open_layers(&mut h);
    let text = screen_text(&h.screen);
    // The title is the proof that `open_layers` itself ran — Task 11's tests
    // stopped at the posted command and at `transform`, before the opener.
    assert!(text.contains("Layers — alpine:3.19"), "{text}");
    assert!(text.contains("FROM alpine"), "{text}");
    assert!(text.contains("RUN apk add"), "{text}");
    let requests = count_layer_requests(&mut h);
    assert_eq!(requests, 0, "a cached analysis is never re-requested");
}

/// The uncached path of the same open: no cached analysis ⇒ the window
/// asks the worker exactly once, keyed by the image id.
#[test]
fn an_uncached_image_asks_the_worker_exactly_once() {
    let mut h = app_start(seed_image);
    open_layers(&mut h);
    let requests = count_layer_requests(&mut h);
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
/// and the CHANGE cell is delimited by spaces, so `contains(" - ")` is
/// exactly "this row's letter is `-`" (the PERM column has its own
/// dashes, but never surrounded by spaces).
#[test]
fn a_whiteout_renders_as_a_deletion_on_the_removed_paths_row() {
    let mut h = app_start(|core| seed(core, whiteout_analysis()));
    open_layers(&mut h);
    // Layer 0 against itself marks nothing: the tombstone must not show yet.
    let text = screen_text(&h.screen);
    assert!(
        !line_with(&text, "hosts").contains(" - "),
        "layer 0 wrote hosts and nothing removed it yet: {text}"
    );
    // One Down: the cursor reaches layer 1 and the tree rebuilds for it.
    h.screen.push_key(Key::Down, KeyModifiers::default());
    pump(&mut h.program, 2);
    let text = screen_text(&h.screen);
    assert!(
        line_with(&text, "hosts").contains(" - "),
        "the whiteout renders as a deletion: {text}"
    );
    assert!(
        !line_with(&text, "passwd").contains(" - "),
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
    // (every rendered tree row carries the fixture's `-rw-r--r--`
    // permission string; the layer table has none, so the count is the
    // rendered tree-row count).
    h.screen.push_key(Key::Char('a'), KeyModifiers::default());
    pump(&mut h.program, 2);
    let text = screen_text(&h.screen);
    assert!(
        !line_with(&text, "RUN apk add b").contains('+'),
        "the Added filter hid /b: {text}"
    );
    assert_eq!(
        text.matches("-rw-r--r--").count(),
        1,
        "only the Unmodified /a row survives the filter: {text}"
    );
    // Both keys toggle back — a one-way toggle must fail here.
    h.screen.push_key(Key::Char('a'), KeyModifiers::default());
    pump(&mut h.program, 2);
    let text = screen_text(&h.screen);
    assert_eq!(
        text.matches("-rw-r--r--").count(),
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

/// A tree of `n` files under `prefix`, each named `b##`. The tag lets a test
/// read WHICH pane scrolled straight off the screen.
fn tree_of_many(prefix: &str, n: usize) -> Tree {
    let mut t = Tree::new();
    for i in 0..n {
        t.add_path(entry(&format!("/{prefix}/b{i:02}"), "v1"));
    }
    t
}

/// 30 layers whose COMMAND cells are `A00..A29`, each holding the SAME 30
/// `b##` files. Identical trees matter: wheeling the layer pane moves its
/// cursor, and `selected` follows the cursor, so the tree pane rebuilds. With
/// every layer's tree equal, that rebuild yields the same 30 rows — so a
/// change in ONE pane can only have come from the wheel, never from the
/// selection moving underneath the assertion.
fn many_layer_analysis() -> Analysis {
    analysis_of(
        (0..30)
            .map(|i| {
                layer(
                    i,
                    &format!("A{i:02}"),
                    1_000_000,
                    tree_of_many("d", 30),
                    &[],
                )
            })
            .collect(),
    )
}

/// The lowest `A##`/`b##` tag still on screen — that pane's
/// top-of-viewport row, or `None` when the pane shows none of them.
fn top_visible(text: &str, tag: u8) -> Option<usize> {
    (0..60).find(|i| text.contains(&format!("{}{:02}", tag as char, i)))
}

/// Wheel down `times`. The group fans the wheel to every child without
/// hit-testing, so the position only has to be plausible.
fn wheel_down(h: &mut common::AppHarness, times: usize) {
    for _ in 0..times {
        h.screen
            .push_event(Event::MouseWheel(tv::event::MouseEvent {
                position: tv::Point::new(10, 10),
                wheel: tv::event::MouseWheel::Down,
                ..tv::event::MouseEvent::default()
            }));
        // Each tick steps the bar, which broadcasts, which queues a
        // ScrollSync deferred — a few pumps to settle. A real wheel tick is
        // far slower than this loop, so this models the gap, not starves it.
        pump(&mut h.program, 8);
    }
}

/// The wheel must scroll the pane that holds the focus.
///
/// `Group::handle_event` fans `MouseWheel` out to every child in REVERSE
/// insertion order and stops at the first consumer, and only the `ScrollBar`s
/// handle it. This window inserts `layer_bar` before `tree_bar`, so the tree
/// bar was reached first and consumed every scroll — the layer pane could not
/// receive a wheel at all, exactly as the Explorer's host pane could not.
#[test]
fn the_mouse_wheel_scrolls_the_focused_pane() {
    let mut h = app_start(|core| seed(core, many_layer_analysis()));
    open_layers(&mut h);
    let seeded = screen_text(&h.screen);
    assert_eq!(
        top_visible(&seeded, b'A'),
        Some(0),
        "the layer pane starts at its top: {seeded}"
    );
    assert_eq!(
        top_visible(&seeded, b'b'),
        Some(0),
        "the tree pane starts at its top: {seeded}"
    );

    // The layer table owns the focus on open (`request_initial_focus`), so
    // the wheel belongs to it. 8 ticks x 3 rows clears the ~16-row viewport.
    wheel_down(&mut h, 8);
    let after_left = screen_text(&h.screen);
    let left_top = top_visible(&after_left, b'A');
    assert!(
        left_top.is_some_and(|t| t > 0),
        "the wheel scrolled the FOCUSED layer pane (top={left_top:?}): {after_left}"
    );
    assert_eq!(
        top_visible(&after_left, b'b'),
        Some(0),
        "…and left the unfocused tree pane alone: {after_left}"
    );

    // Tab crosses to the tree table: now the wheel must scroll IT, and the
    // layer pane must stay exactly where it was.
    h.screen.push_key(Key::Tab, KeyModifiers::default());
    pump(&mut h.program, 4);
    wheel_down(&mut h, 8);
    let after_right = screen_text(&h.screen);
    let right_top = top_visible(&after_right, b'b');
    assert!(
        right_top.is_some_and(|t| t > 0),
        "the wheel scrolled the FOCUSED tree pane (top={right_top:?}): {after_right}"
    );
    assert_eq!(
        top_visible(&after_right, b'A'),
        left_top,
        "…and left the layer pane where it was: {after_right}"
    );
}
