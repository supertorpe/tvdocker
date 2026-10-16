//! The Details window ("Parsed inspect view: metadata, env,
//! volumes, networks, ports, labels; scrollable"): one window PER
//! container over `AppCore::details` — Enter re-inspects, the window
//! renders its own container's latest payload (amended
//! for ×N). The body is a non-following `LogView` with a right-edge
//! vertical scroll bar; there is no status row, so the loading state
//! lives in the body itself.

use crate::core::AppCore;
use crate::widgets::LogView;
use crate::widgets::pane::ScrollWindow;
use std::cell::RefCell;
use std::rc::Rc;
use tv::{Context, Rect, View, ViewState, delegate};

/// Window-menu number for Details.
const WINDOW_NUMBER: i16 = 7;

/// Body text while no inspect payload is cached (there is no status row).
const LOADING: &str = " Loading container details…";

/// The Details window: `ScrollWindow` (body + right scroll bar, no search
/// or status line) whose body is a non-following `LogView` — a document,
/// scrolled with the standard keys. One per container; `open` is cleared
/// on drop so the slot can reopen it.
pub struct DetailsWindow {
    win: ScrollWindow,
    core: Rc<RefCell<AppCore>>,
    id: String,
    last_rev: u64,
    open: Rc<RefCell<bool>>,
}

/// Build the window for one container; primes from the cached payload if
/// one exists (reopen while the data is still current).
pub fn build(
    core: Rc<RefCell<AppCore>>,
    bounds: Rect,
    id: &str,
    name: &str,
    open: Rc<RefCell<bool>>,
) -> DetailsWindow {
    let mut win = ScrollWindow::new(
        bounds,
        format!("Details — {name}"),
        WINDOW_NUMBER,
        |rect, bar| {
            let mut log = LogView::new(rect, bar);
            log.set_follow(false); // no tail on a document
            Box::new(log)
        },
    );
    let last_rev = core.borrow().details_rev;
    let payload = core.borrow().details.get(id).map(|d| d.lines.clone());
    if let Some(log) = log_mut(&mut win) {
        load_body(log, payload);
    }
    DetailsWindow {
        win,
        core,
        id: id.to_string(),
        last_rev,
        open,
    }
}

/// Replace the body with `payload` (the cached inspect lines), or with the
/// loading line when there is none.
fn load_body(log: &mut LogView, payload: Option<Vec<String>>) {
    log.clear();
    match payload {
        Some(lines) => {
            for line in lines {
                log.push_line(line);
            }
        }
        None => log.push_line(LOADING),
    }
}

fn log_mut(win: &mut ScrollWindow) -> Option<&mut LogView> {
    let body = win.ids().body;
    win.window_mut()
        .child_mut(body)
        .and_then(|v| v.as_any_mut().and_then(|a| a.downcast_mut::<LogView>()))
}

impl Drop for DetailsWindow {
    /// Clear the slot's open flag so closing the window lets Enter reopen
    /// it (mirrors `ResourceWindow`; the stale-ViewId focus path would
    /// otherwise silently no-op).
    fn drop(&mut self) {
        *self.open.borrow_mut() = false;
    }
}

#[delegate(to = win)]
impl View for DetailsWindow {
    fn state(&self) -> &ViewState {
        self.win.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.win.state_mut()
    }

    fn handle_event(&mut self, ev: &mut tv::event::Event, ctx: &mut Context) {
        self.win.handle_event(ev, ctx);
        // Re-render when a new inspect lands (or clears to loading).
        let rev = self.core.borrow().details_rev;
        if rev != self.last_rev {
            self.last_rev = rev;
            let payload = self
                .core
                .borrow()
                .details
                .get(&self.id)
                .map(|d| d.lines.clone());
            if let Some(log) = log_mut(&mut self.win) {
                load_body(log, payload);
            }
        }
        // Publish the content length to the scroll bar (follow is off, so this
        // never moves the viewport).
        if let Some(log) = log_mut(&mut self.win) {
            log.sync(ctx);
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::AppMsg;
    use crate::domain::docker::details::Details;

    fn core() -> Rc<RefCell<AppCore>> {
        Rc::new(RefCell::new(AppCore::new()))
    }

    /// Build a Details window for `id`; returns the window and its shared
    /// open flag (cleared on drop so the slot can reopen).
    fn window(core: Rc<RefCell<AppCore>>, id: &str) -> (DetailsWindow, Rc<RefCell<bool>>) {
        let open = Rc::new(RefCell::new(true));
        let win = build(core, Rect::new(2, 1, 78, 23), id, "web", Rc::clone(&open));
        (win, open)
    }

    fn bump(core: &Rc<RefCell<AppCore>>, id: &str, name: &str, lines: Vec<&str>) {
        core.borrow_mut().apply(AppMsg::Details {
            id: id.to_string(),
            details: Details {
                name: name.to_string(),
                lines: lines.into_iter().map(str::to_string).collect(),
            },
        });
    }

    /// Any event pushes the window through its rev check (the LogView
    /// consumes arrows harmlessly).
    fn tick(w: &mut DetailsWindow) {
        crate::testutil::send_key(w, tv::event::Key::Down);
    }

    #[test]
    fn renders_loading_then_the_payload_and_reloads_on_rev() {
        let core = core();
        let (mut w, _open) = window(core.clone(), "id-web");
        let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
        assert!(
            out.contains("Loading container details"),
            "empty core ⇒ loading state: {out:?}"
        );

        bump(
            &core,
            "id-web",
            "web",
            vec![" Name", "  web", " ENVIRONMENT", "  A=B"],
        );
        tick(&mut w);
        let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
        assert!(out.contains("  A=B"), "payload rendered: {out:?}");
        assert!(out.contains("web"), "name on the status row: {out:?}");

        // A second inspect for the SAME container REPLACES the render —
        // the clear() path, not an append.
        bump(&core, "id-web", "web", vec![" Name", "  web2"]);
        tick(&mut w);
        let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
        assert!(out.contains("  web2"), "replaced: {out:?}");
        assert!(!out.contains("  A=B"), "old payload gone: {out:?}");
    }

    #[test]
    fn a_window_renders_only_its_own_container() {
        let core = core();
        let (mut w, _open) = window(core.clone(), "id-web");
        bump(&core, "id-web", "web", vec![" Name", "  web"]);
        tick(&mut w);
        // Another container's inspect must not leak into this window (×N).
        bump(&core, "id-db", "db", vec![" Name", "  db"]);
        tick(&mut w);
        let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
        assert!(out.contains("  web"), "own payload kept: {out:?}");
        assert!(
            !out.contains("  db"),
            "other container's payload absent: {out:?}"
        );
    }

    #[test]
    fn a_reopened_window_primes_from_the_cached_payload() {
        let core = core();
        bump(&core, "id-web", "web", vec![" Name", "  web"]);
        let (mut w, _open) = window(core, "id-web");
        // No event yet — the build primed from core.
        let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
        assert!(out.contains("  web"), "primed at build: {out:?}");
    }

    #[test]
    fn drop_clears_the_slot_open_flag() {
        let core = core();
        let (w, open) = window(core, "id-web");
        assert!(*open.borrow(), "open while the window lives");
        drop(w);
        assert!(!*open.borrow(), "close ⇒ the slot can reopen it");
    }
}
