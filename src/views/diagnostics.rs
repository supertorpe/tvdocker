//! The Diagnostics window: one window per container,
//! a tvision-rs `Splitter` with the analysis left and the playbook right.
//! The window owns the two shared line-vectors the panes read; it rewrites
//! them when `AppCore::diagnostics_rev` moves.
use crate::core::{AppCore, DiagnosticsPhase};
use crate::widgets::pane::CompactWindow;
use crate::widgets::{SharedLine, TextPane};
use std::cell::RefCell;
use std::rc::Rc;
use tv::{Constraints, Context, DrawCtx, Rect, Role, Splitter, View, ViewId, ViewState, delegate};

/// Window-menu number for Diagnostics (after Explorer = 10).
const WINDOW_NUMBER: i16 = 11;

/// Status-line context for the Diagnostics window .
pub const HELP_CTX: tv::HelpCtx = tv::HelpCtx::custom("tvdocker.diagnostics");

/// The status-row text for a phase.
pub fn phase_label(phase: &DiagnosticsPhase) -> String {
    match phase {
        DiagnosticsPhase::Collecting => " Collecting context…".to_string(),
        DiagnosticsPhase::Analyzing => " Analyzing with the configured model…".to_string(),
        DiagnosticsPhase::Done => " Done — ↑↓ PgUp/PgDn scroll · Tab switches pane".to_string(),
        DiagnosticsPhase::Error(msg) => format!(" Error: {msg}"),
    }
}

/// One container's Diagnostics window (×N;).
pub struct DiagnosticsWindow {
    win: CompactWindow,
    core: Rc<RefCell<AppCore>>,
    id: String,
    last_rev: u64,
    open: Rc<RefCell<bool>>,
    /// The status row's shared text (drawn by a `SharedLine` child).
    status: Rc<RefCell<String>>,
    /// The two panes' shared line-vectors (the window is the mutator).
    analysis_lines: Rc<RefCell<Vec<String>>>,
    playbook_lines: Rc<RefCell<Vec<String>>>,
    /// The two pane ids (filled by the body closure) — for initial focus.
    pane_ids: Rc<RefCell<Option<(ViewId, ViewId)>>>,
}

/// Build one container's Diagnostics window; primes from the cached run if
/// one exists (reopen while the data is still current).
pub fn build(
    core: Rc<RefCell<AppCore>>,
    bounds: Rect,
    id: &str,
    name: &str,
    open: Rc<RefCell<bool>>,
) -> DiagnosticsWindow {
    let status = Rc::new(RefCell::new(" Collecting context…".to_string()));
    let analysis_lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let playbook_lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let pane_ids: Rc<RefCell<Option<(ViewId, ViewId)>>> = Rc::new(RefCell::new(None));

    let al = Rc::clone(&analysis_lines);
    let pl = Rc::clone(&playbook_lines);
    let pid = Rc::clone(&pane_ids);
    let mut win = CompactWindow::new(
        bounds,
        format!("Diagnostics — {name}"),
        WINDOW_NUMBER,
        Box::new(SharedLine::new(
            Rect::new(0, 0, 1, 1),
            Rc::clone(&status),
            Role::Info,
        )),
        move |rect| {
            let mut split = Splitter::cols().joined();
            let a = split.insert(
                Box::new(TextPane::new(Rect::new(0, 0, 1, 1), Rc::clone(&al), true)),
                Constraints::flex(),
            );
            let p = split.insert(
                Box::new(TextPane::new(Rect::new(0, 0, 1, 1), Rc::clone(&pl), false)),
                Constraints::flex(),
            );
            *pid.borrow_mut() = Some((a, p));
            split.change_bounds(rect);
            Box::new(split)
        },
    );
    win.state_mut().help_ctx = HELP_CTX;

    let last_rev = core.borrow().diagnostics_rev;
    let mut window = DiagnosticsWindow {
        win,
        core,
        id: id.to_string(),
        last_rev,
        open,
        status,
        analysis_lines,
        playbook_lines,
        pane_ids,
    };
    window.refresh_from_core(); // prime (reopen while current, or fresh run)
    window
}

impl DiagnosticsWindow {
    /// Pull the current run for `self.id` into the shared state. Called at
    /// build and whenever `diagnostics_rev` moves.
    fn refresh_from_core(&mut self) {
        let (phase, analysis, playbook) = {
            let core = self.core.borrow();
            match core.diagnostics.get(&self.id) {
                Some(st) => (st.phase.clone(), st.analysis.clone(), st.playbook.clone()),
                None => (DiagnosticsPhase::Collecting, String::new(), String::new()),
            }
        };
        *self.status.borrow_mut() = phase_label(&phase);
        *self.analysis_lines.borrow_mut() = split_lines(&analysis);
        *self.playbook_lines.borrow_mut() = split_lines(&playbook);
    }

    /// The analysis pane's id, once the body closure has run (initial focus).
    pub fn analysis_pane(&self) -> Option<ViewId> {
        self.pane_ids.borrow().map(|(a, _)| a)
    }

    /// The playbook pane's id (the right pane; the mirror of
    /// [`analysis_pane`](Self::analysis_pane) for focus assertions).
    pub fn playbook_pane(&self) -> Option<ViewId> {
        self.pane_ids.borrow().map(|(_, p)| p)
    }

    /// Ask the pump to focus the ANALYSIS pane once wired in — a non-modal
    /// window's `reset_current` is never called, so the settle pass would
    /// otherwise pick the last-inserted selectable child (ruling 4).
    pub fn request_initial_focus(&mut self, ctx: &mut Context) {
        if let Some((analysis, _)) = *self.pane_ids.borrow() {
            ctx.request_focus(analysis);
        }
    }
}

/// Split prose into display lines (a plain newline split; empty text ⇒ no
/// lines).
fn split_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.lines().map(str::to_string).collect()
}

impl Drop for DiagnosticsWindow {
    fn drop(&mut self) {
        *self.open.borrow_mut() = false;
    }
}

#[delegate(to = win)]
impl View for DiagnosticsWindow {
    fn state(&self) -> &ViewState {
        self.win.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.win.state_mut()
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.win.draw(ctx);
    }

    fn handle_event(&mut self, ev: &mut tv::event::Event, ctx: &mut Context) {
        self.win.handle_event(ev, ctx);
        let rev = self.core.borrow().diagnostics_rev;
        if rev != self.last_rev {
            self.last_rev = rev;
            self.refresh_from_core();
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

    fn core_with(id: &str) -> (Rc<RefCell<AppCore>>, u64) {
        let core = Rc::new(RefCell::new(AppCore::new()));
        let run = core.borrow_mut().start_diagnostics(id);
        (core, run)
    }

    fn window(core: Rc<RefCell<AppCore>>, id: &str) -> (DiagnosticsWindow, Rc<RefCell<bool>>) {
        let open = Rc::new(RefCell::new(true));
        let w = build(core, Rect::new(2, 1, 78, 23), id, "web", Rc::clone(&open));
        (w, open)
    }

    fn tick(w: &mut DiagnosticsWindow) {
        crate::testutil::send_key(w, tv::Key::Down);
    }

    #[test]
    fn renders_analysis_and_playbook_after_a_run() {
        let (core, run) = core_with("id-web");
        core.borrow_mut().apply(AppMsg::DiagnosticsAnalysis {
            id: "id-web".into(),
            run,
            text: "disk is full".into(),
        });
        core.borrow_mut().apply(AppMsg::DiagnosticsPlaybook {
            id: "id-web".into(),
            run,
            text: "Step 1: prune images".into(),
        });
        let (mut w, _open) = window(core, "id-web");
        tick(&mut w); // prime from the rev
        let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
        assert!(out.contains("disk is full"), "{out}");
        assert!(out.contains("Step 1: prune images"), "{out}");
    }

    /// Review Focus #3.
    #[test]
    fn a_window_renders_only_its_own_run() {
        let core = Rc::new(RefCell::new(AppCore::new()));
        let a = core.borrow_mut().start_diagnostics("id-a");
        let b = core.borrow_mut().start_diagnostics("id-b");
        core.borrow_mut().apply(AppMsg::DiagnosticsAnalysis {
            id: "id-a".into(),
            run: a,
            text: "AAA".into(),
        });
        core.borrow_mut().apply(AppMsg::DiagnosticsAnalysis {
            id: "id-b".into(),
            run: b,
            text: "BBB".into(),
        });
        let (mut w, _open) = window(core, "id-a");
        tick(&mut w);
        let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
        assert!(out.contains("AAA"), "{out}");
        assert!(!out.contains("BBB"), "no cross-container leak: {out}");
    }

    /// Review Focus #5.
    #[test]
    fn drop_clears_the_slot_open_flag() {
        let (core, _run) = core_with("id-web");
        let (w, open) = window(core, "id-web");
        assert!(*open.borrow());
        drop(w);
        assert!(!*open.borrow(), "close ⇒ the slot can reopen it");
    }

    /// Review Focus #2 (integration): an error phase reaches the status row.
    #[test]
    fn an_error_phase_shows_in_the_status_row() {
        let (core, run) = core_with("id-web");
        core.borrow_mut().apply(AppMsg::DiagnosticsError {
            id: "id-web".into(),
            run,
            message: "collect id-web: no such container".into(),
        });
        let (mut w, _open) = window(core, "id-web");
        tick(&mut w);
        let out = crate::testutil::text(&crate::testutil::render_view(76, 23, &mut w));
        assert!(
            out.contains("no such container") || out.contains("Error"),
            "{out}"
        );
    }

    /// Review Focus #1 — `request_initial_focus` targets the ANALYSIS pane
    /// (the first-inserted child; the pump's settle pass would otherwise pick
    /// the last-inserted, playbook). Phase 4's opener must call it.
    #[test]
    fn opening_focuses_the_analysis_pane() {
        let (core, _run) = core_with("id-web");
        let (mut w, _open) = window(core, "id-web");
        let analysis = w.analysis_pane().expect("body closure captured the ids");
        let mut out: std::collections::VecDeque<tv::Event> = std::collections::VecDeque::new();
        let mut timers = tv::TimerQueue::new();
        let mut deferred: Vec<tv::Deferred> = Vec::new();
        {
            let mut ctx = tv::Context::new(&mut out, &mut timers, 0, &mut deferred);
            w.request_initial_focus(&mut ctx);
        }
        assert!(
            deferred
                .iter()
                .any(|d| matches!(d, tv::Deferred::FocusById(id) if *id == analysis)),
            "the analysis pane is focused"
        );
    }

    /// Review Focus #4 — drive the resize the repo's way (`change_bounds` +
    /// render; tvision-rs 0.16 has no `Event::Resize`). Non-vacuous: the
    /// splitter body must refill the new client and the panes must re-lay-out
    /// (a frozen child would leave both assertions failing).
    #[test]
    fn panes_follow_a_resize() {
        let (core, run) = core_with("id-web");
        core.borrow_mut().apply(AppMsg::DiagnosticsAnalysis {
            id: "id-web".into(),
            run,
            text: "A".into(),
        });
        let (mut w, _open) = window(core, "id-web");
        let _ = crate::testutil::render_view(76, 23, &mut w);
        let analysis = w.analysis_pane().expect("body closure captured the ids");
        let before = pane_bounds(&mut w, analysis);

        w.change_bounds(Rect::new(2, 1, 40, 12));
        let _ = crate::testutil::render_view(40, 12, &mut w);

        let body = w.win.ids().body;
        let body_bounds = w
            .win
            .window_mut()
            .child_mut(body)
            .expect("splitter body")
            .state()
            .get_bounds();
        let want_body = crate::widgets::pane::compact_rects(w.win.state().size).body;
        assert_eq!(
            body_bounds, want_body,
            "the splitter body refills the new client area"
        );
        let after = pane_bounds(&mut w, analysis);
        assert!(
            after.b.x < before.b.x,
            "the analysis pane narrowed with the window: {before:?} -> {after:?}"
        );
    }

    /// A pane's current bounds, reached window → splitter body → pane.
    fn pane_bounds(w: &mut DiagnosticsWindow, id: ViewId) -> Rect {
        let body = w.win.ids().body;
        w.win
            .window_mut()
            .child_mut(body)
            .expect("splitter body")
            .find_mut(id)
            .expect("pane id")
            .state()
            .get_bounds()
    }

    /// Tab switches panes .
    ///
    /// Ruling evidence for phase 4's status-def decision: `key_item(Tab)`
    /// is deliberately NOT bound, because the `Group` handles Tab itself.
    /// NOTE: the pane switch cannot be asserted in this unit context —
    /// `Group::set_current` only propagates the `Focused` flag when the
    /// group itself is focused, and a unit context has no pump to settle
    /// currency (so `route_event` has no `current` child to deliver to).
    /// Pinned instead by the integration test
    /// `tab_switches_which_pane_the_scroll_keys_reach`, which drives the
    /// real pump.
    #[test]
    fn tab_switching_is_covered_by_the_integration_pump() {
        // The window builds with exactly two selectable panes, so the
        // framework's Tab traversal has both stops available.
        let (core, _run) = core_with("id-web");
        let (mut w, _open) = window(core, "id-web");
        let _ = crate::testutil::render_view(76, 23, &mut w);
        let analysis = w.analysis_pane().expect("pane ids");
        let playbook = w.playbook_pane().expect("pane ids");
        assert_ne!(analysis, playbook, "two distinct panes");
        assert!(
            pane_bounds(&mut w, analysis).b.x < pane_bounds(&mut w, playbook).a.x,
            "analysis is the LEFT pane"
        );
    }
}
