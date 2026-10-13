//! Dynamic status line : tvision-rs lays items out left-to-right
//! only (verified `status_line.rs`), so the right side — Docker state +
//! counts — is painted by this wrapper AFTER the embedded StatusLine draws.
//!
//! Composition: a Group whose child is the real StatusLine —
//! the child keeps its ViewId, so the gray-cache regray broadcast and
//! `request_update_menu` keep working; the wrapper forwards the
//! `View::update_menu_commands` trait hook the pump dispatches on the
//! STORED id (program.rs:554). The pre-route reaches the wrapper by id
//! (program.rs:2217 → `group.find_mut` → direct `handle_event`), and the
//! group's pre-phase reaches the inner line (its `pre_process` flag).
//! `as_any_mut` yields the EMBEDDED line's
//! `Any`, so the pump's `set_help_ctx` downcast (program.rs:2195 —
//! the only status downcast) lands and contextual defs follow the
//! focused window.
use crate::core::{AppCore, Connection};
use std::cell::RefCell;
use std::rc::Rc;
use tv::{DrawCtx, Group, Rect, Role, StatusLine, View, ViewState, delegate};

/// Right-side text for the current connection state (green
/// connected / amber retrying); the neutral `Checking` state shows
/// "checking…" instead.
pub fn connection_label(core: &AppCore) -> Option<(String, Role)> {
    let (running, total) = core.counts();
    match &core.connection {
        Connection::Checking => Some(("checking…".to_string(), Role::Info)),
        Connection::Up => Some((format!("connected {running}/{total}"), Role::Success)),
        Connection::Down { .. } => Some(("retrying…".to_string(), Role::Warning)),
    }
}

/// The status line + right-side state .
pub struct DynamicStatus {
    group: Group,
    inner_id: tv::ViewId,
    core: Rc<RefCell<AppCore>>,
    /// Last known size of the wrapper; a reload rebuilds the embedded line
    /// at this size (the line is re-created, so it needs fresh bounds).
    size: tv::Point,
}

impl DynamicStatus {
    /// Wrap a StatusLine built by `app::init_status_line` so the stored
    /// status id still resolves to a view whose `update_menu_commands`
    /// reaches the line (see module docs).
    pub fn wrap(mut inner: StatusLine, core: Rc<RefCell<AppCore>>) -> Self {
        let bounds = inner.state().get_bounds();
        let mut group = Group::new(bounds);
        // Mirror the line's grow modes so the WRAPPER tracks the bottom
        // edge and stretches exactly like the StatusLine would at the root
        // level (resize keeps the right-side text on the status row).
        group.state_mut().grow_mode = inner.state().grow_mode;
        let w = bounds.b.x - bounds.a.x;
        let h = bounds.b.y - bounds.a.y;
        inner.state_mut().set_bounds(Rect::new(0, 0, w, h));
        let inner_id = group.insert(Box::new(inner));
        Self {
            group,
            inner_id,
            core,
            size: tv::Point::new(w, h),
        }
    }

    /// Rebuild the embedded status line from the current config (settings
    /// live reload on save). The inner `ViewId` changes, so re-store
    /// it — `set_help_ctx` and `update_menu_commands` resolve through it
    /// (module docs).
    pub fn rebuild(&mut self, ctx: &mut tv::Context) {
        let (w, h) = (self.size.x, self.size.y);
        self.group.remove(self.inner_id, ctx);
        let mut line = match crate::app::init_status_line(Rect::new(0, h - 1, w, h)) {
            Some(l) => l,
            None => return,
        };
        line.state_mut().set_bounds(Rect::new(0, 0, w, h));
        self.inner_id = self.group.insert(Box::new(line));
    }

    /// The embedded line's id — a test hook so a reload can be observed
    /// without widening the public surface.
    #[cfg(test)]
    fn inner_id_for_test(&self) -> tv::ViewId {
        self.inner_id
    }

    /// The embedded status line (test/introspection hook).
    pub fn inner_status(&mut self) -> Option<&mut StatusLine> {
        self.group
            .child_mut(self.inner_id)
            .and_then(|v| v.as_any_mut())
            .and_then(|a| a.downcast_mut::<StatusLine>())
    }
}

#[delegate(to = group)]
impl View for DynamicStatus {
    fn state(&self) -> &ViewState {
        self.group.state()
    }
    fn state_mut(&mut self) -> &mut ViewState {
        self.group.state_mut()
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.group.draw(ctx); // left items (fills the row first)
        let core = self.core.borrow();
        // Precedence : latched error ⚠ > latched info > the
        // connection label. Long messages CLAMP to the row — a status
        // line must never wrap onto a second row.
        let (label, role) = if let Some(err) = core.last_error.as_deref() {
            (format!("⚠ {err}"), Role::Warning)
        } else if let Some(info) = core.info.as_deref() {
            (info.to_string(), Role::Info)
        } else if let Some(pair) = connection_label(&core) {
            pair
        } else {
            return;
        };
        let bounds = self.state().get_bounds();
        let width = bounds.b.x - bounds.a.x;
        let shown: String = label.chars().take((width - 1).max(0) as usize).collect();
        let x = (width - shown.chars().count() as i32 - 1).max(0);
        ctx.put_str(x, 0, &shown, ctx.style(role));
    }

    /// The pump primes/regrays the STORED status id — forward it.
    fn update_menu_commands(&mut self, cmds: &tv::CommandSet) {
        if let Some(inner) = self.inner_status() {
            inner.update_menu_commands(cmds);
        }
    }

    fn handle_event(&mut self, ev: &mut tv::event::Event, ctx: &mut tv::Context) {
        // A Settings save broadcasts a reload; rebuild the embedded line so
        // `[keybindings]` overrides take effect without a restart .
        if let tv::event::Event::Broadcast { command, .. } = ev
            && *command == crate::app::CMD_RELOAD_STATUS
        {
            self.rebuild(ctx);
            // NOT `ev.clear()`: a broadcast is delivered to every view, and
            // `Group::route_event` stops at the first cleared one — clearing
            // here would skip every sibling status line (see the test).
            return;
        }
        // Deliver straight to the embedded line — do NOT route through
        // `Group::handle_event`: `Group::deliver` hands the child a CLONE
        // and only propagates a `clear()` back, so the StatusLine's
        // in-place KeyDown → Command transform (the pre-route contract)
        // would be lost in the clone (observed: F10 never opened the File
        // popup through the wrapper). Direct forwarding is exactly what a
        // root-level StatusLine gets from the pre-route: the LIVE event,
        // mutated in place, unconsumed events left for normal dispatch.
        if let Some(inner) = self.inner_status() {
            inner.handle_event(ev, ctx);
        }
    }

    /// Track the wrapper's size so a later rebuild knows the row's width
    /// (the delegated forward alone would not).
    fn change_bounds(&mut self, bounds: Rect) {
        self.size = tv::Point::new(bounds.b.x - bounds.a.x, bounds.b.y - bounds.a.y);
        self.group.change_bounds(bounds);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        // Hand the pump the EMBEDDED line: everything else the
        // pump does with the stored status id is a trait call this
        // wrapper forwards (`handle_event`, `update_menu_commands`).
        self.inner_status().map(|s| s as &mut dyn std::any::Any)
    }
}

/// Build the dynamic status from the app's defs (`app::init_status_line`
/// returns the bare line; this wraps it with the shared core).
pub fn dynamic_status(r: Rect, core: Rc<RefCell<AppCore>>) -> Option<Box<dyn View>> {
    let inner = crate::app::init_status_line(r)?;
    Some(Box::new(DynamicStatus::wrap(inner, core)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AppCore, AppMsg, Connection};
    use crate::testutil::{render_view, text};

    fn core() -> Rc<RefCell<AppCore>> {
        Rc::new(RefCell::new(AppCore::new()))
    }

    fn summary_row(state: &str) -> crate::domain::docker::containers::ContainerSummary {
        crate::domain::docker::containers::ContainerSummary {
            id: state.to_string(),
            name: format!("c-{state}"),
            image: "img".into(),
            state: state.into(),
            status: "Up".into(),
            ports: String::new(),
            project: String::new(),
            health: String::new(),
        }
    }

    /// Reach the embedded StatusLine through the wrapper — after the
    /// `as_any_mut` fix the wrapper's Any IS the inner
    /// line's, which is exactly what the pump relies on.
    fn inner_status(v: &mut dyn View) -> Option<&mut StatusLine> {
        v.as_any_mut()?.downcast_mut::<StatusLine>()
    }

    #[test]
    fn connection_label_covers_all_states() {
        let c = core();
        assert!(
            connection_label(&c.borrow())
                .unwrap()
                .0
                .contains("checking"),
            "startup"
        );
        c.borrow_mut().apply(AppMsg::Connection(Connection::Up));
        assert_eq!(connection_label(&c.borrow()).unwrap().1, Role::Success);
        c.borrow_mut()
            .apply(AppMsg::Connection(Connection::Down { error: "e".into() }));
        let (text, role) = connection_label(&c.borrow()).unwrap();
        assert!(text.contains("retrying"), "amber state: {text}");
        assert_eq!(role, Role::Warning);
    }

    #[test]
    fn right_side_shows_state_and_counts_and_never_touches_left_items() {
        let c = core();
        c.borrow_mut().apply(AppMsg::Connection(Connection::Up));
        c.borrow_mut().apply(AppMsg::Containers(vec![
            summary_row("running"),
            summary_row("exited"),
        ]));
        let mut v = dynamic_status(Rect::new(0, 0, 80, 1), Rc::clone(&c)).unwrap();
        let out = text(&render_view(80, 1, &mut *v));
        assert!(out.contains("1/2"), "counts on the right: {out:?}");
        assert!(out.contains("connected"), "state on the right: {out:?}");
        assert!(out.contains("F1 Help"), "left items still drawn: {out:?}");
        // Right-aligned: the state text sits in the RIGHT half of the row.
        let row = out.lines().next().unwrap_or_default();
        let state_at = row.find("connected").unwrap();
        assert!(
            state_at > 40,
            "right-aligned, not glued to the left items: {row:?}"
        );
    }

    #[test]
    fn status_wrapper_keeps_the_pre_route() {
        // Through the wrapper, F10 must still transform to Command::MENU
        // and open the File popup (the pre-route delivers to the STORED
        // status id — our wrapper — whose group routes to the line).
        let (backend, screen) = tv::HeadlessBackend::new(80, 25);
        let c = core();
        let mut program = crate::app::program_with_status(
            Box::new(backend),
            Box::new(tv::SystemClock::new()),
            |r| {
                let mut r = r;
                r.a.y += 1;
                r.b.y -= 1;
                Some(Box::new(tv::Desktop::new(r, |br| {
                    Some(tv::Desktop::init_background(br))
                })) as Box<dyn tv::View>)
            },
            {
                let core = Rc::clone(&c);
                move |r| dynamic_status(r, core)
            },
        );
        screen.push_key(tv::event::Key::F(10), tv::event::KeyModifiers::default());
        for _ in 0..3 {
            program.pump_once();
        }
        screen.push_key(tv::event::Key::Enter, tv::event::KeyModifiers::default());
        for _ in 0..3 {
            program.pump_once();
        }
        let out = text(&screen);
        // NOT just `contains("Exit")`: the status row itself always shows
        // "Alt-X Exit" — that made the original assertion vacuous (the
        // popup-through-wrapper breakage shipped right past it). The popup
        // opens BELOW the menu bar; the status row is row 24.
        let popup = out
            .lines()
            .take(3)
            .position(|l| l.contains("Exit"))
            .expect("File popup below the menu bar (not the status row)");
        assert!(popup <= 2, "popup at row {popup}: {out:?}");
    }

    #[test]
    fn status_forward_commands_keeps_graying_data_flow() {
        // The pump dispatches `View::update_menu_commands` on the STORED
        // status id (our wrapper) — it must reach the inner StatusLine so
        // the pre-route's `command_enabled` sees the live disabled set.
        let c = core();
        let mut v = dynamic_status(Rect::new(0, 0, 80, 1), Rc::clone(&c)).unwrap();
        let mut disabled = tv::CommandSet::new();
        disabled.insert(tv::Command::CLOSE);
        v.update_menu_commands(&disabled);
        let inner = inner_status(&mut *v).expect("inner status line");
        // `command_enabled` is private; it reads exactly this public cache
        // (`!disabled.is_some_and(|cs| cs.has(cmd))`), so asserting the
        // forwarded set is asserting the pre-route's input.
        let cmds = inner.disabled_cmds().expect("cache primed by the forward");
        assert!(cmds.has(tv::Command::CLOSE), "graying data forwarded");
        assert!(!cmds.has(tv::Command::QUIT), "QUIT stays enabled");
    }

    #[test]
    fn wrapper_as_any_mut_is_the_embedded_line() {
        // The pump's idle arm downcasts the STORED status id to
        // `StatusLine` (program.rs:2195 — the only such site).
        // Through this wrapper the downcast must LAND or contextual defs
        // never activate.
        let c = core();
        let mut v = dynamic_status(Rect::new(0, 0, 80, 1), Rc::clone(&c)).unwrap();
        let any = v.as_any_mut().expect("wrapper exposes Any");
        assert!(
            any.downcast_mut::<StatusLine>().is_some(),
            "as_any_mut must yield the inner StatusLine"
        );
    }

    #[test]
    fn right_side_shows_error_then_info_then_connection() {
        let c = core();
        let mut v = dynamic_status(Rect::new(0, 0, 80, 1), Rc::clone(&c)).unwrap();
        let out = text(&render_view(80, 1, v.as_mut()));
        assert!(out.contains("checking"), "baseline: {out:?}");

        c.borrow_mut()
            .apply(AppMsg::Status("Pruned 3 unused images".into()));
        let mut v = dynamic_status(Rect::new(0, 0, 80, 1), Rc::clone(&c)).unwrap();
        let out = text(&render_view(80, 1, v.as_mut()));
        assert!(
            out.contains("Pruned 3 unused images"),
            "info shown: {out:?}"
        );
        assert!(!out.contains('⚠'), "info carries NO marker: {out:?}");

        c.borrow_mut()
            .apply(AppMsg::TaskError("remove image: no such image".into()));
        let mut v = dynamic_status(Rect::new(0, 0, 80, 1), Rc::clone(&c)).unwrap();
        let out = text(&render_view(80, 1, v.as_mut()));
        assert!(out.contains('⚠'), "error is marked: {out:?}");
        assert!(out.contains("remove image"), "error text: {out:?}");
        assert!(
            !out.contains("Pruned 3"),
            "the error supersedes the info line: {out:?}"
        );
    }

    #[test]
    fn a_long_right_side_message_clamps_instead_of_wrapping() {
        let c = core();
        c.borrow_mut().apply(AppMsg::Status("x".repeat(200)));
        let mut v = dynamic_status(Rect::new(0, 0, 80, 2), Rc::clone(&c)).unwrap();
        let out = text(&render_view(80, 2, v.as_mut()));
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(rows.len(), 2, "two rendered rows: {out:?}");
        // Row 1 must stay BLANK — a wrapped remainder would land there.
        assert!(
            rows[1].trim().is_empty(),
            "the message clamps to row 0, never wraps: {out:?}"
        );
        assert!(
            rows[0].trim_end().ends_with('x'),
            "the clamp cut the tail at the row's right edge: {rows:?}"
        );
    }

    /// The reload broadcast is handled but NOT consumed: `Group::route_event`
    /// stops delivering a broadcast at the first view that clears it, so
    /// consuming would skip every sibling status line — and `CMD_RELOAD_STATUS`
    /// is documented as "there may be a status line per screen". The house
    /// convention (`log_view`'s broadcast arm) leaves broadcasts live.
    #[test]
    fn the_reload_broadcast_stays_live_and_still_rebuilds() {
        let core = Rc::new(RefCell::new(AppCore::new()));
        let mut status = crate::app::init_status_line(Rect::new(0, 24, 80, 25))
            .map(|l| super::DynamicStatus::wrap(l, Rc::clone(&core)))
            .expect("status");
        let before = status.inner_id_for_test();
        let mut ev = tv::event::Event::Broadcast {
            command: crate::app::CMD_RELOAD_STATUS,
            source: None,
        };
        crate::testutil::with_ctx(|ctx| status.handle_event(&mut ev, ctx));
        assert_ne!(before, status.inner_id_for_test(), "the line was rebuilt");
        assert!(
            !matches!(ev, tv::event::Event::Command(_)),
            "the broadcast must stay live for sibling status lines"
        );
        assert!(
            matches!(ev, tv::event::Event::Broadcast { .. }),
            "and nothing else should consume it either"
        );
    }

    /// The reload broadcast rebuilds the embedded line, and the wrapper's
    /// inner id is re-stored so the pump's `set_help_ctx` downcast still
    /// lands (module docs).
    #[test]
    fn rebuild_swaps_the_inner_line_and_restores_the_id() {
        let core = Rc::new(RefCell::new(AppCore::new()));
        let mut status = crate::app::init_status_line(Rect::new(0, 24, 80, 25))
            .map(|l| super::DynamicStatus::wrap(l, Rc::clone(&core)))
            .expect("status");
        let before = status.inner_id_for_test();
        crate::testutil::with_ctx(|ctx| status.rebuild(ctx));
        let after = status.inner_id_for_test();
        assert_ne!(before, after, "a fresh line was inserted");
        assert!(
            status.state().get_bounds().b.y > 0,
            "the rebuilt line has bounds"
        );
    }
}
