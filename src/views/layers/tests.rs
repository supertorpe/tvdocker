use super::*;
use crate::domain::layers::{Analysis, Entry, Kind, Layer, Tree, hash_bytes};
use crate::testutil;

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

/// Sync from the core AND push into the tables — a test-side shim for what
/// `handle_event` does with a real `Context`.
fn sync_and_draw(win: &mut LayersWindow) {
    win.sync_from_core();
    crate::testutil::with_ctx(|ctx| win.redraw(ctx));
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
