//! Standard pane layout: one-row info, one-row filter/search, body + right
//! scrollbar, all strictly inside the window frame.
//! Child rects are DERIVED from the window size — never hardcoded — and
//! re-applied whenever the window size changes (round-2 layout review).

#[cfg(test)]
mod tests {
    use super::*;
    use tv::{View, ViewState};

    /// Client area = everything strictly inside the 1-cell frame.
    fn client(size: Point) -> Rect {
        Rect::new(1, 1, size.x - 1, size.y - 1)
    }

    #[test]
    fn pane_rects_are_inside_the_client_area() {
        for (w, h) in [(76, 22), (74, 22), (52, 16), (120, 40)] {
            let size = Point::new(w, h);
            let c = client(size);
            let r = pane_rects(size);
            for (name, rect) in [
                ("info", r.info),
                ("filter", r.filter),
                ("body", r.body),
                ("bar", r.bar),
            ] {
                assert!(
                    rect.a.x >= c.a.x
                        && rect.a.y >= c.a.y
                        && rect.b.x <= c.b.x
                        && rect.b.y <= c.b.y,
                    "{name} {rect:?} escapes client {c:?} ({w}x{h})"
                );
                assert!(
                    rect.b.x >= rect.a.x && rect.b.y >= rect.a.y,
                    "{name} inverted: {rect:?}"
                );
            }
        }
    }

    #[test]
    fn pane_rects_shape_for_the_probe_window() {
        // table_probe window is 76x22 → client (1,1)-(75,21).
        let r = pane_rects(Point::new(76, 22));
        assert_eq!(
            r.info,
            Rect::new(1, 1, 75, 2),
            "info is one row at client top"
        );
        assert_eq!(r.filter, Rect::new(1, 2, 75, 3), "filter is one row below");
        assert_eq!(
            r.body,
            Rect::new(1, 3, 74, 21),
            "body stops left of the bar, above the frame"
        );
        assert_eq!(
            r.bar,
            Rect::new(74, 3, 75, 21),
            "bar is the rightmost client column"
        );
        // The frame cells must be untouched (rects are half-open: the last
        // INCLUDED cell of the body is b-1, which must sit strictly inside
        // frame col75 / row21):
        assert!(
            r.body.b.x - 1 < 75 && r.body.b.y - 1 < 21,
            "body must not touch frame col75/row21"
        );
        assert_eq!(
            r.bar.b.x - 1,
            74,
            "bar's last cell is the rightmost CLIENT column"
        );
    }

    #[test]
    fn pane_rects_degenerate_sizes_do_not_panic() {
        for (w, h) in [(1, 1), (3, 3), (5, 4), (4, 6), (2, 10)] {
            let r = pane_rects(Point::new(w, h));
            for rect in [r.info, r.filter, r.body, r.bar] {
                assert!(
                    rect.b.x >= rect.a.x && rect.b.y >= rect.a.y,
                    "{w}x{h}: inverted {rect:?}"
                );
            }
        }
    }

    #[test]
    fn compact_rects_keep_status_and_body_inside_the_frame() {
        // pty_probe window is 76x22 → client (1,1)-(75,21).
        let r = compact_rects(Point::new(76, 22));
        assert_eq!(
            r.status,
            Rect::new(1, 20, 75, 21),
            "status is the last CLIENT row (frame is 21)"
        );
        assert_eq!(
            r.body,
            Rect::new(1, 1, 75, 20),
            "body stops one row above the status"
        );
        assert!(
            r.body.b.y <= r.status.a.y,
            "body and status must be disjoint"
        );
        for (name, rect) in [("status", r.status), ("body", r.body)] {
            assert!(
                rect.b.x <= 75 && rect.b.y <= 21,
                "{name} escapes client: {rect:?}"
            );
            assert!(
                rect.b.x >= rect.a.x && rect.b.y >= rect.a.y,
                "{name} inverted: {rect:?}"
            );
        }
    }

    #[test]
    fn compact_rects_degenerate_sizes_do_not_panic() {
        for (w, h) in [(1, 1), (2, 2), (3, 3), (5, 4), (76, 2)] {
            let r = compact_rects(Point::new(w, h));
            for rect in [r.status, r.body] {
                assert!(
                    rect.b.x >= rect.a.x && rect.b.y >= rect.a.y,
                    "{w}x{h}: inverted {rect:?}"
                );
            }
        }
    }

    #[test]
    fn scroll_rects_keep_body_and_bar_inside_the_frame() {
        // 76x22 → client (1,1)-(75,21); the bar is the rightmost client column,
        // the body everything left of it, both from the client top.
        let r = scroll_rects(Point::new(76, 22));
        assert_eq!(
            r.body,
            Rect::new(1, 1, 74, 21),
            "body left of the bar, above the frame"
        );
        assert_eq!(
            r.bar,
            Rect::new(74, 1, 75, 21),
            "bar is the rightmost client column"
        );
        for (name, rect) in [("body", r.body), ("bar", r.bar)] {
            assert!(
                rect.a.x >= 1 && rect.a.y >= 1 && rect.b.x <= 75 && rect.b.y <= 21,
                "{name} escapes client: {rect:?}"
            );
        }
    }

    #[test]
    fn scroll_rects_degenerate_sizes_do_not_panic() {
        for (w, h) in [(1, 1), (2, 2), (3, 3), (5, 4), (76, 2)] {
            let r = scroll_rects(Point::new(w, h));
            for rect in [r.body, r.bar] {
                assert!(
                    rect.b.x >= rect.a.x && rect.b.y >= rect.a.y,
                    "{w}x{h}: inverted {rect:?}"
                );
            }
        }
    }

    #[test]
    fn scroll_window_relayouts_children_after_a_resize() {
        use crate::testutil::render_view;
        let (win, ids) =
            build_scroll_window(Rect::new(2, 1, 78, 23), "details", 7, |rect, _bar| {
                let mut d = dummy();
                d.state_mut().set_bounds(rect);
                d
            });
        let mut cw = ScrollWindow { window: win, ids };
        let _ = render_view(80, 25, &mut cw);
        cw.change_bounds(Rect::new(2, 1, 60, 12));
        let _ = render_view(80, 25, &mut cw);
        let want = scroll_rects(cw.state().size);
        for (id, want) in [(cw.ids.body, want.body), (cw.ids.bar, want.bar)] {
            let got = cw.window_mut().child_mut(id).unwrap().state().get_bounds();
            assert_eq!(got, want, "child refills the shrunk window's client");
        }
    }

    #[test]
    fn scroll_status_rects_keep_status_body_and_bar_inside_the_frame() {
        // 76x22 → client (1,1)-(75,21); the status row is the last client
        // line, the bar the rightmost client column above it, the body the
        // rest.
        let r = scroll_status_rects(Point::new(76, 22));
        assert_eq!(
            r.status,
            Rect::new(1, 20, 75, 21),
            "status spans the client, one row up from the frame"
        );
        assert_eq!(
            r.body,
            Rect::new(1, 1, 74, 20),
            "body left of the bar, above the status"
        );
        assert_eq!(
            r.bar,
            Rect::new(74, 1, 75, 20),
            "bar is the rightmost client column"
        );
        assert_eq!(
            r.body.b.y, r.status.a.y,
            "body stops where the status starts"
        );
        assert_eq!(r.body.b.x, r.bar.a.x, "body stops where the bar starts");
        for (name, rect) in [("status", r.status), ("body", r.body), ("bar", r.bar)] {
            assert!(
                rect.a.x >= 1 && rect.a.y >= 1 && rect.b.x <= 75 && rect.b.y <= 21,
                "{name} escapes client: {rect:?}"
            );
            assert!(
                rect.b.x >= rect.a.x && rect.b.y >= rect.a.y,
                "{name} inverted: {rect:?}"
            );
        }
    }

    #[test]
    fn scroll_status_rects_degenerate_sizes_do_not_panic() {
        for (w, h) in [(1, 1), (2, 2), (3, 3), (5, 4), (76, 2)] {
            let r = scroll_status_rects(Point::new(w, h));
            for rect in [r.status, r.body, r.bar] {
                assert!(
                    rect.b.x >= rect.a.x && rect.b.y >= rect.a.y,
                    "{w}x{h}: inverted {rect:?}"
                );
            }
        }
    }

    #[test]
    fn scroll_status_window_hands_the_body_its_bar_and_relayouts_on_resize() {
        use crate::testutil::render_view;
        let seen_bar = std::cell::Cell::new(None);
        let (win, ids) = build_scroll_status_window(
            Rect::new(2, 1, 78, 23),
            "shell",
            9,
            dummy(),
            |rect, bar| {
                seen_bar.set(bar);
                let mut d = dummy();
                d.state_mut().set_bounds(rect);
                d
            },
        );
        assert_eq!(
            seen_bar.get(),
            Some(ids.bar),
            "the body is linked to the bar the layout inserted"
        );
        let mut win = win;
        assert!(
            !win.child_mut(ids.bar).unwrap().state().options.selectable,
            "the bar must not stop the Tab cycle"
        );

        let mut cw = ScrollStatusWindow { window: win, ids };
        let _ = render_view(80, 25, &mut cw);
        cw.change_bounds(Rect::new(2, 1, 60, 12));
        let _ = render_view(80, 25, &mut cw);
        let want = scroll_status_rects(cw.state().size);
        for (id, want) in [
            (cw.ids.status, want.status),
            (cw.ids.body, want.body),
            (cw.ids.bar, want.bar),
        ] {
            let got = cw.window_mut().child_mut(id).unwrap().state().get_bounds();
            assert_eq!(got, want, "child refills the shrunk window's client");
        }
    }

    #[test]
    fn compact_window_relayouts_children_after_a_resize() {
        use crate::testutil::render_view;
        let (win, ids) =
            build_compact_window(Rect::new(2, 1, 78, 23), "shell", 1, dummy(), |rect| {
                let mut d = dummy();
                d.state_mut().set_bounds(rect);
                d
            });
        let mut cw = CompactWindow { window: win, ids };
        let _ = render_view(80, 25, &mut cw);
        cw.change_bounds(Rect::new(2, 1, 60, 12));
        let _ = render_view(80, 25, &mut cw);
        let want = compact_rects(cw.state().size);
        let body_id = cw.ids.body;
        let got = cw
            .window_mut()
            .child_mut(body_id)
            .unwrap()
            .state()
            .get_bounds();
        assert_eq!(got, want.body, "body refills the shrunk window's client");
        let status_id = cw.ids.status;
        let got = cw
            .window_mut()
            .child_mut(status_id)
            .unwrap()
            .state()
            .get_bounds();
        assert_eq!(got, want.status, "status row tracks the shrink");
    }

    /// Drive `CMD_FULLSCREEN` through a wrapper's real `handle_event` with a
    /// desktop-sized owner (the pump sets this via the desktop group's phase
    /// bracket). Returns whether the wrapper consumed the event.
    fn press_fullscreen(w: &mut dyn View) -> bool {
        let mut consumed = false;
        crate::testutil::with_ctx(|ctx| {
            ctx.set_owner_size(Point::new(80, 25));
            let mut ev = tv::event::Event::Command(crate::app::CMD_FULLSCREEN);
            w.handle_event(&mut ev, ctx);
            consumed = ev.is_nothing();
        });
        consumed
    }

    /// The full-screen toggle reaches the inner window through PaneWindow:
    /// enter Desktop frameless, revert to Off with the frame and the
    /// pre-fullscreen bounds back — and the pane children follow both ways.
    #[test]
    fn full_screen_toggles_the_pane_window_and_children_follow() {
        use tv::Fullscreen;
        let orig = Rect::new(2, 1, 78, 23);
        let (win, ids) = build_pane_window(orig, "probe", 1, dummy(), dummy(), |rect, _bar| {
            let mut d = dummy();
            d.state_mut().set_bounds(rect);
            d
        });
        let mut pane = PaneWindow { window: win, ids };
        let body = pane.ids.body;

        assert!(
            press_fullscreen(&mut pane),
            "PaneWindow consumes CMD_FULLSCREEN"
        );
        assert_eq!(
            pane.window_mut().fullscreen(),
            Fullscreen::Desktop,
            "first press enters Desktop"
        );
        assert_eq!(
            pane.state().get_bounds(),
            Rect::new(0, 0, 80, 25),
            "the window fills its owner"
        );
        assert!(!pane.window_mut().bordered(), "and drops the frame");
        let want = pane_rects(pane.state().size);
        assert_eq!(
            pane.window_mut()
                .child_mut(body)
                .unwrap()
                .state()
                .get_bounds(),
            want.body,
            "pane children re-lay out to the fullscreen size"
        );

        assert!(
            press_fullscreen(&mut pane),
            "the second press is consumed too"
        );
        assert_eq!(
            pane.window_mut().fullscreen(),
            Fullscreen::Off,
            "one press reverts (no Screen hop)"
        );
        assert!(pane.window_mut().bordered(), "the frame comes back");
        assert_eq!(
            pane.state().get_bounds(),
            orig,
            "the pre-fullscreen bounds are restored"
        );
        let want = pane_rects(pane.state().size);
        assert_eq!(
            pane.window_mut()
                .child_mut(body)
                .unwrap()
                .state()
                .get_bounds(),
            want.body,
            "children relayout back to the framed client"
        );
    }

    /// The same toggle through CompactWindow (the Diagnostics shell window).
    #[test]
    fn full_screen_toggles_the_compact_window() {
        use tv::Fullscreen;
        let orig = Rect::new(2, 1, 78, 23);
        let (win, ids) = build_compact_window(orig, "diag", 1, dummy(), |rect| {
            let mut d = dummy();
            d.state_mut().set_bounds(rect);
            d
        });
        let mut cw = CompactWindow { window: win, ids };

        assert!(press_fullscreen(&mut cw), "CompactWindow consumes it");
        assert_eq!(cw.window_mut().fullscreen(), Fullscreen::Desktop);
        assert!(!cw.window_mut().bordered());
        assert_eq!(cw.state().get_bounds(), Rect::new(0, 0, 80, 25));

        assert!(press_fullscreen(&mut cw));
        assert_eq!(cw.window_mut().fullscreen(), Fullscreen::Off);
        assert!(cw.window_mut().bordered());
        assert_eq!(cw.state().get_bounds(), orig, "bounds restored");
    }

    /// The same toggle through ScrollWindow (the Details document window).
    #[test]
    fn full_screen_toggles_the_scroll_window() {
        use tv::Fullscreen;
        let orig = Rect::new(2, 1, 78, 23);
        let (win, ids) = build_scroll_window(orig, "details", 7, |rect, _bar| {
            let mut d = dummy();
            d.state_mut().set_bounds(rect);
            d
        });
        let mut sw = ScrollWindow { window: win, ids };

        assert!(press_fullscreen(&mut sw), "ScrollWindow consumes it");
        assert_eq!(sw.window_mut().fullscreen(), Fullscreen::Desktop);
        assert_eq!(sw.state().get_bounds(), Rect::new(0, 0, 80, 25));

        assert!(press_fullscreen(&mut sw));
        assert_eq!(sw.window_mut().fullscreen(), Fullscreen::Off);
        assert_eq!(sw.state().get_bounds(), orig, "bounds restored");
    }

    /// The same toggle through ScrollStatusWindow (the shell window, which the
    /// desktop inserts directly — no outer wrapper).
    #[test]
    fn full_screen_toggles_the_scroll_status_window() {
        use tv::Fullscreen;
        let orig = Rect::new(2, 1, 78, 23);
        let (win, ids) = build_scroll_status_window(orig, "shell", 9, dummy(), |rect, _bar| {
            let mut d = dummy();
            d.state_mut().set_bounds(rect);
            d
        });
        let mut sw = ScrollStatusWindow { window: win, ids };

        assert!(press_fullscreen(&mut sw), "ScrollStatusWindow consumes it");
        assert_eq!(sw.window_mut().fullscreen(), Fullscreen::Desktop);
        assert_eq!(sw.state().get_bounds(), Rect::new(0, 0, 80, 25));

        assert!(press_fullscreen(&mut sw));
        assert_eq!(sw.window_mut().fullscreen(), Fullscreen::Off);
        assert_eq!(sw.state().get_bounds(), orig, "bounds restored");
    }

    #[test]
    fn pane_scroll_bar_is_not_selectable_so_tab_skips_it() {
        // Tab cycles filter ↔ table; the bar must not stop the trip.
        let (win, ids) = build_pane_window(
            Rect::new(0, 0, 40, 12),
            "w",
            1,
            dummy(),
            dummy(),
            |rect, _bar| {
                let mut d = dummy();
                d.state_mut().set_bounds(rect);
                d
            },
        );
        let mut win = win;
        let selectable = win.child_mut(ids.bar).unwrap().state().options.selectable;
        assert!(
            !selectable,
            "scroll bar participates in neither Tab order nor focus"
        );
        let _ = &mut win;
    }

    struct Dummy {
        st: ViewState,
    }
    impl View for Dummy {
        fn state(&self) -> &tv::ViewState {
            &self.st
        }
        fn state_mut(&mut self) -> &mut tv::ViewState {
            &mut self.st
        }
        fn draw(&mut self, _ctx: &mut tv::DrawCtx) {}
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
    }

    fn dummy() -> Box<dyn View> {
        Box::new(Dummy {
            st: ViewState::new(Rect::new(0, 0, 1, 1)),
        })
    }

    #[test]
    fn build_pane_window_places_children_at_the_derived_rects() {
        let (mut win, ids) = build_pane_window(
            Rect::new(2, 1, 78, 23),
            "probe",
            1,
            dummy(),
            dummy(),
            |rect, _bar| {
                let mut d = dummy();
                d.state_mut().set_bounds(rect);
                d
            },
        );
        let r = pane_rects(win.state().size);
        assert_eq!(
            win.child_mut(ids.info).unwrap().state().get_bounds(),
            r.info
        );
        assert_eq!(
            win.child_mut(ids.filter).unwrap().state().get_bounds(),
            r.filter
        );
        assert_eq!(
            win.child_mut(ids.body).unwrap().state().get_bounds(),
            r.body
        );
        assert_eq!(win.child_mut(ids.bar).unwrap().state().get_bounds(), r.bar);
        let _ = &mut win;
    }

    #[test]
    fn pane_window_relayouts_children_after_a_resize() {
        use crate::testutil::render_view;
        let (win, ids) = build_pane_window(
            Rect::new(2, 1, 78, 23),
            "probe",
            1,
            dummy(),
            dummy(),
            |rect, _bar| {
                let mut d = dummy();
                d.state_mut().set_bounds(rect);
                d
            },
        );
        let mut pane = PaneWindow { window: win, ids };
        // Simulate the window growing (zoom / drag) and redraw — children
        // must follow the new size via `change_bounds`.
        let _ = render_view(80, 25, &mut pane);
        let body_id = pane.ids.body;
        let grown = Rect::new(2, 1, 60, 16);
        pane.change_bounds(grown);
        let _ = render_view(80, 25, &mut pane);
        let want = pane_rects(pane.state().size);
        let got = pane
            .window_mut()
            .child_mut(body_id)
            .unwrap()
            .state()
            .get_bounds();
        assert_eq!(
            got, want.body,
            "body must refill the grown window's client area"
        );
    }
}

use tv::widgets::{SortDir, Table};
use tv::{Context, DrawCtx, Point, Rect, ScrollBar, View, ViewId, Window, delegate};

/// Child rectangles for the standard pane, derived from the WINDOW size
/// (frame included): the frame owns row `h-1` and column `w-1`, so the
/// client is `(1,1)-(w-1,h-1)` exclusive and no child may touch it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRects {
    /// One-row info/stats line at the client top.
    pub info: Rect,
    /// One-row filter/search line below it.
    pub filter: Rect,
    /// Main content: below `filter`, left of `bar`, above the frame.
    pub body: Rect,
    /// Rightmost client column, from the body top to the frame.
    pub bar: Rect,
}

/// Derive the pane layout for a window of `size`.
/// Degenerate sizes yield empty-but-never-inverted rects.
pub fn pane_rects(size: Point) -> PaneRects {
    let right = (size.x - 1).max(1); // exclusive client right (frame = size.x-1)
    let bottom = (size.y - 1).max(1); // exclusive client bottom (frame = size.y-1)
    let col_off = (size.x - 2).max(1); // body's exclusive right / bar's start col
    let row0 = 3.min(bottom); // body top: below info+filter, clamped when tiny
    PaneRects {
        info: Rect::new(1, 1, right, 2.min(bottom)),
        filter: Rect::new(1, 2.min(bottom), right, 3.min(bottom)),
        body: Rect::new(1, row0, col_off, bottom),
        bar: Rect::new(col_off, row0, right, bottom),
    }
}

/// The inserted children of a pane window (ids stamped at build time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneIds {
    pub info: ViewId,
    pub filter: ViewId,
    pub bar: ViewId,
    pub body: ViewId,
}

/// Build a window whose children sit at [`pane_rects`] positions.
/// `body` receives its rect and the already-inserted scroll-bar id
/// (tvdir bars-first pattern: `Some(bar_id)` for Table/LogView).
pub fn build_pane_window(
    bounds: Rect,
    title: impl Into<String>,
    number: i16,
    info: Box<dyn View>,
    filter: Box<dyn View>,
    body: impl FnOnce(Rect, Option<ViewId>) -> Box<dyn View>,
) -> (Window, PaneIds) {
    let mut win = Window::new(bounds, Some(title.into()), number);
    win.state_mut().options.tileable = true;
    let r = pane_rects(win.state().size);

    let mut info = info;
    info.state_mut().set_bounds(r.info);
    let info_id = win.insert_child(info);

    let mut filter = filter;
    filter.state_mut().set_bounds(r.filter);
    let filter_id = win.insert_child(filter);

    let bar_id = win.insert_child(Box::new(ScrollBar::new(r.bar)));
    let body_id = win.insert_child(body(r.body, Some(bar_id)));

    (
        win,
        PaneIds {
            info: info_id,
            filter: filter_id,
            bar: bar_id,
            body: body_id,
        },
    )
}

fn apply_pane_layout(win: &mut Window, ids: &PaneIds) {
    let r = pane_rects(win.state().size);
    for (id, rect) in [
        (ids.info, r.info),
        (ids.filter, r.filter),
        (ids.bar, r.bar),
        (ids.body, r.body),
    ] {
        if let Some(view) = win.child_mut(id) {
            view.change_bounds(rect);
        }
    }
}

/// Window wrapper that keeps its children laid out from the CURRENT window
/// size: every resize path (zoom / Size-Move drag / terminal clamps) goes
/// through [`View::change_bounds`], which re-applies the layout as soon as the
/// new bounds land — so the pane always fills the client area and never
/// overlaps the frame (the round-2 layout fix).
pub struct PaneWindow {
    window: Window,
    ids: PaneIds,
}

impl PaneWindow {
    pub fn new(
        bounds: Rect,
        title: impl Into<String>,
        number: i16,
        info: Box<dyn View>,
        filter: Box<dyn View>,
        body: impl FnOnce(Rect, Option<ViewId>) -> Box<dyn View>,
    ) -> Self {
        let (window, ids) = build_pane_window(bounds, title, number, info, filter, body);
        Self { window, ids }
    }

    /// The inserted children (probes downcast the body by id).
    pub fn ids(&self) -> &PaneIds {
        &self.ids
    }

    pub fn window_mut(&mut self) -> &mut Window {
        &mut self.window
    }
}

#[delegate(to = window)]
impl View for PaneWindow {
    fn state(&self) -> &tv::ViewState {
        self.window.state()
    }
    fn state_mut(&mut self) -> &mut tv::ViewState {
        self.window.state_mut()
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.window.draw(ctx);
    }

    fn change_bounds(&mut self, bounds: Rect) {
        self.window.change_bounds(bounds);
        apply_pane_layout(&mut self.window, &self.ids);
    }

    fn handle_event(&mut self, ev: &mut tv::event::Event, ctx: &mut tv::Context) {
        let before = self.window.state().size;
        // Windows ▸ Full screen: toggle HERE — this wrapper owns the inner
        // window, so the command never reaches (or is swallowed by) children.
        let full = crate::app::take_fullscreen(ev, &mut self.window, ctx);
        if !full {
            self.window.handle_event(ev, ctx);
        }
        // Zoom / fullscreen re-bound the inner window's group directly
        // (`Window::maximize` -> `locate` -> `group.change_bounds`), which never
        // reaches this wrapper's `change_bounds`; reconcile so the pane children
        // (and their scroll bars) follow the new client area. The toggle also
        // runs the reconcile when only the BORDER flipped (size unchanged):
        // `set_bordered`'s grow-mode reflow would otherwise leave the children
        // where the frame used to be.
        if full || self.window.state().size != before {
            apply_pane_layout(&mut self.window, &self.ids);
            self.window.on_bounds_changed(ctx);
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// Child rectangles for a compact pane (status row + body), e.g. the shell
/// window: the status gets the LAST client row and the body everything above
/// it, so terminal content (and the hardware cursor) can never collide with
/// the status label, and neither ever touches the frame .
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactRects {
    /// One-row status line at the bottom of the client area.
    pub status: Rect,
    /// Everything between the client top and the status row.
    pub body: Rect,
}

/// Derive the compact layout for a window of `size` (frame included).
/// Degenerate sizes yield empty-but-never-inverted rects.
pub fn compact_rects(size: Point) -> CompactRects {
    let right = (size.x - 1).max(1); // exclusive client right (frame = size.x-1)
    let bottom = (size.y - 1).max(1); // exclusive client bottom (frame = size.y-1)
    let status_y = (bottom - 1).max(1); // last client row (frame is `bottom`)
    CompactRects {
        status: Rect::new(1, status_y, right, bottom.max(status_y)),
        body: Rect::new(1, 1, right, status_y.max(1)),
    }
}

/// The inserted children of a compact window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactIds {
    pub status: ViewId,
    pub body: ViewId,
}

/// Build a window with a status row and a body (tvdir bars-first not needed:
/// the body closure receives no bar id — compact panes have no scrollbar).
pub fn build_compact_window(
    bounds: Rect,
    title: impl Into<String>,
    number: i16,
    status: Box<dyn View>,
    body: impl FnOnce(Rect) -> Box<dyn View>,
) -> (Window, CompactIds) {
    let mut win = Window::new(bounds, Some(title.into()), number);
    win.state_mut().options.tileable = true;
    let r = compact_rects(win.state().size);

    let mut status = status;
    status.state_mut().set_bounds(r.status);
    let status_id = win.insert_child(status);

    let body_id = win.insert_child(body(r.body));

    (
        win,
        CompactIds {
            status: status_id,
            body: body_id,
        },
    )
}

fn apply_compact_layout(win: &mut Window, ids: &CompactIds) {
    let r = compact_rects(win.state().size);
    for (id, rect) in [(ids.status, r.status), (ids.body, r.body)] {
        if let Some(view) = win.child_mut(id) {
            view.change_bounds(rect);
        }
    }
}

/// Window wrapper for the compact pane: re-derives the status/body rects on
/// every [`View::change_bounds`] (zoom / Size-Move / terminal clamps) — the
/// round-2 fix for the shell window's cursor-outside-window bug.
pub struct CompactWindow {
    window: Window,
    ids: CompactIds,
}

impl CompactWindow {
    pub fn new(
        bounds: Rect,
        title: impl Into<String>,
        number: i16,
        status: Box<dyn View>,
        body: impl FnOnce(Rect) -> Box<dyn View>,
    ) -> Self {
        let (window, ids) = build_compact_window(bounds, title, number, status, body);
        Self { window, ids }
    }

    pub fn ids(&self) -> &CompactIds {
        &self.ids
    }

    pub fn window_mut(&mut self) -> &mut Window {
        &mut self.window
    }
}

#[delegate(to = window)]
impl View for CompactWindow {
    fn state(&self) -> &tv::ViewState {
        self.window.state()
    }
    fn state_mut(&mut self) -> &mut tv::ViewState {
        self.window.state_mut()
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.window.draw(ctx);
    }

    fn change_bounds(&mut self, bounds: Rect) {
        self.window.change_bounds(bounds);
        apply_compact_layout(&mut self.window, &self.ids);
    }

    fn handle_event(&mut self, ev: &mut tv::event::Event, ctx: &mut tv::Context) {
        let before = self.window.state().size;
        // Windows ▸ Full screen: this wrapper owns the inner window.
        let full = crate::app::take_fullscreen(ev, &mut self.window, ctx);
        if !full {
            self.window.handle_event(ev, ctx);
        }
        // Zoom / fullscreen re-bound the inner window's group directly
        // (`Window::maximize` -> `locate` -> `group.change_bounds`), which never
        // reaches this wrapper's `change_bounds`; reconcile so the compact
        // children follow the new client area (also after a border-only flip).
        if full || self.window.state().size != before {
            apply_compact_layout(&mut self.window, &self.ids);
            self.window.on_bounds_changed(ctx);
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// Child rectangles for a scroll window (body + right scroll bar): no info,
/// filter or status rows, so the body fills the client area and the bar is the
/// rightmost client column. The Details document lives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollRects {
    /// Main content: the whole client area left of `bar`.
    pub body: Rect,
    /// Rightmost client column, from the client top to the frame.
    pub bar: Rect,
}

/// Derive the scroll layout for a window of `size` (frame included).
/// Degenerate sizes yield empty-but-never-inverted rects.
pub fn scroll_rects(size: Point) -> ScrollRects {
    let right = (size.x - 1).max(1); // exclusive client right (frame = size.x-1)
    let bottom = (size.y - 1).max(1); // exclusive client bottom (frame = size.y-1)
    let col_off = (size.x - 2).max(1); // body's exclusive right / bar's start col
    ScrollRects {
        body: Rect::new(1, 1, col_off, bottom),
        bar: Rect::new(col_off, 1, right, bottom),
    }
}

/// The inserted children of a scroll window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollIds {
    pub body: ViewId,
    pub bar: ViewId,
}

/// Build a window whose client is one right-edge scroll bar and a body filling
/// the rest. The body closure receives its rect and the already-inserted bar id
/// (tvdir bars-first pattern, like [`build_pane_window`]).
pub fn build_scroll_window(
    bounds: Rect,
    title: impl Into<String>,
    number: i16,
    body: impl FnOnce(Rect, Option<ViewId>) -> Box<dyn View>,
) -> (Window, ScrollIds) {
    let mut win = Window::new(bounds, Some(title.into()), number);
    win.state_mut().options.tileable = true;
    let r = scroll_rects(win.state().size);

    let bar_id = win.insert_child(Box::new(ScrollBar::new(r.bar)));
    let body_id = win.insert_child(body(r.body, Some(bar_id)));

    (
        win,
        ScrollIds {
            body: body_id,
            bar: bar_id,
        },
    )
}

fn apply_scroll_layout(win: &mut Window, ids: &ScrollIds) {
    let r = scroll_rects(win.state().size);
    for (id, rect) in [(ids.bar, r.bar), (ids.body, r.body)] {
        if let Some(view) = win.child_mut(id) {
            view.change_bounds(rect);
        }
    }
}

/// Deliver a mouse wheel to one specific scroll bar, then re-assert the value
/// the bar just chose. Returns `true` when `ev` WAS a wheel — the event is
/// consumed, so the caller must `return` without delegating it to its children.
///
/// Two windows with side-by-side scroll bars need this, and neither can rely on
/// the default routing:
///
/// * **Routing.** `Group::handle_event` fans `MouseWheel` out to every child in
///   REVERSE insertion order and stops at the first consumer, and only
///   `ScrollBar`s handle the wheel. Whichever bar was inserted LAST therefore
///   wins every scroll no matter which pane holds the focus — the Explorer's
///   host pane and the Layers pane both went dead while the right-hand bar
///   responded. Calling this with the FOCUSED pane's bar makes focus decide.
///
/// * **Stall.** The wheel makes the BAR the source of truth, but the list
///   viewer's cursor disagrees, and `list_viewer::focus_item` reconciles by
///   pushing the cursor row back into the bar — also deferred, and queued a
///   pass EARLIER, so in the deferred drain that write-back lands after ours
///   and wins. The bar snaps back to the cursor row, the sync reads the stale
///   cursor, and the pane never scrolls past it: every tick went `3 -> 6 ->
///   3`, forever stuck on the first step. Re-asserting our value as a deferred
///   puts ours LAST in the drain, so the sync that follows reads OUR value and
///   the cursor catches up instead of fighting back.
///
/// The bar ignores the position for the wheel (it only reads the direction),
/// but the coordinates are localized into the bar's frame anyway — matching
/// what `Group::deliver` would have done — so a future position-aware bar
/// still behaves.
pub fn wheel_scrolls_bar(
    win: &mut Window,
    bar: ViewId,
    ev: &mut tv::event::Event,
    ctx: &mut tv::Context,
) -> bool {
    let tv::event::Event::MouseWheel(m) = ev else {
        return false;
    };
    let m = *m;
    let mut local = tv::event::Event::MouseWheel(m);
    let stepped = {
        let Some(child) = win.child_mut(bar) else {
            ev.clear();
            return true;
        };
        if let tv::event::Event::MouseWheel(lm) = &mut local {
            lm.position -= child.state().get_bounds().a;
        }
        child.handle_event(&mut local, ctx);
        child
            .as_any_mut()
            .and_then(|a| a.downcast_mut::<ScrollBar>())
            .map(|sb| sb.value)
    };
    if let Some(value) = stepped {
        ctx.request_scroll_bar_params(bar, Some(value), None, None, None, None);
    }
    ev.clear();
    true
}

/// Step `t`'s sort to the next/previous VISIBLE column — the app-wide ←/→
/// table convention (the F1 box's "Tables" line), shared by the
/// ResourceWindow, the Explorer panels and the three Layers tables. The
/// window owns the cycle because the upstream `Table` command layer only
/// exposes forward-ascending (`TABLE_SORT_NEXT`) and flip
/// (`TABLE_SORT_FLIP`):
///
/// * `forward` steps right (ascending) or left (backward lands
///   descending, as the old table did); a fresh table starts ON its
///   first/last visible column.
/// * `flip` flips the current column's direction instead of stepping
///   (Shift+←/→); with no sort yet it starts ascending on the first
///   visible column.
///
/// Hidden columns are skipped (driven off [`Table::column_layout`]), so
/// the arrows never sort by a column the user hid.
pub fn cycle_table_sort(t: &mut Table, forward: bool, flip: bool, ctx: &mut Context) {
    let layout = t.column_layout();
    let n = layout.len();
    if n == 0 {
        return;
    }
    let mut visible = vec![false; n];
    for s in &layout {
        if s.column < n {
            visible[s.column] = !s.hidden;
        }
    }
    let current = t.sort_spec();
    let (col, dir) = if flip {
        match current {
            Some((c, d)) => (c, d.flip()),
            None => match (0..n).find(|&i| visible[i]) {
                Some(c) => (c, SortDir::Asc),
                None => return,
            },
        }
    } else {
        let step: i64 = if forward { 1 } else { -1 };
        // No sort yet: start one past the first / last column so the first
        // step lands ON it.
        let start = current.map_or(if forward { -1 } else { n as i64 }, |(c, _)| c as i64);
        let mut found = None;
        let mut probe = start;
        for _ in 0..n {
            probe = (probe + step).rem_euclid(n as i64);
            let i = probe as usize;
            if visible[i] {
                found = Some(i);
                break;
            }
        }
        match found {
            Some(c) => (c, if forward { SortDir::Asc } else { SortDir::Desc }),
            None => return,
        }
    };
    t.sort_by(col, dir, ctx);
}

/// Window wrapper for the scroll layout: re-derives the body/bar rects on every
/// [`View::change_bounds`] (zoom / Size-Move / terminal clamps), the
/// [`PaneWindow`] pattern.
pub struct ScrollWindow {
    window: Window,
    ids: ScrollIds,
}

impl ScrollWindow {
    pub fn new(
        bounds: Rect,
        title: impl Into<String>,
        number: i16,
        body: impl FnOnce(Rect, Option<ViewId>) -> Box<dyn View>,
    ) -> Self {
        let (window, ids) = build_scroll_window(bounds, title, number, body);
        Self { window, ids }
    }

    pub fn ids(&self) -> &ScrollIds {
        &self.ids
    }

    pub fn window_mut(&mut self) -> &mut Window {
        &mut self.window
    }
}

#[delegate(to = window)]
impl View for ScrollWindow {
    fn state(&self) -> &tv::ViewState {
        self.window.state()
    }
    fn state_mut(&mut self) -> &mut tv::ViewState {
        self.window.state_mut()
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.window.draw(ctx);
    }

    fn change_bounds(&mut self, bounds: Rect) {
        self.window.change_bounds(bounds);
        apply_scroll_layout(&mut self.window, &self.ids);
    }

    fn handle_event(&mut self, ev: &mut tv::event::Event, ctx: &mut tv::Context) {
        let before = self.window.state().size;
        // Windows ▸ Full screen: this wrapper owns the inner window.
        let full = crate::app::take_fullscreen(ev, &mut self.window, ctx);
        if !full {
            self.window.handle_event(ev, ctx);
        }
        // Zoom / fullscreen re-bound the inner window's group directly
        // (`Window::maximize` -> `locate` -> `group.change_bounds`), which never
        // reaches this wrapper's `change_bounds`; reconcile so the body/bar
        // follow the new client area (also after a border-only flip).
        if full || self.window.state().size != before {
            apply_scroll_layout(&mut self.window, &self.ids);
            self.window.on_bounds_changed(ctx);
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// Child rectangles for a scroll+status window (status row + body + right scroll bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollStatusRects {
    pub status: Rect,
    pub body: Rect,
    pub bar: Rect,
}

/// Derive layout for status + body + right scroll bar (like shell: status bottom, body fills middle-left, bar right edge spanning body).
pub fn scroll_status_rects(size: Point) -> ScrollStatusRects {
    let right = (size.x - 1).max(1);
    let bottom = (size.y - 1).max(1);
    let status_y = (bottom - 1).max(1);
    let col_off = (size.x - 2).max(1);
    ScrollStatusRects {
        status: Rect::new(1, status_y, right, bottom.max(status_y)),
        body: Rect::new(1, 1, col_off, status_y.max(1)),
        bar: Rect::new(col_off, 1, right, status_y.max(1)),
    }
}

/// The inserted children of a scroll+status window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollStatusIds {
    pub status: ViewId,
    pub body: ViewId,
    pub bar: ViewId,
}

pub fn build_scroll_status_window(
    bounds: Rect,
    title: impl Into<String>,
    number: i16,
    status: Box<dyn View>,
    body: impl FnOnce(Rect, Option<ViewId>) -> Box<dyn View>,
) -> (Window, ScrollStatusIds) {
    let mut win = Window::new(bounds, Some(title.into()), number);
    win.state_mut().options.tileable = true;
    let r = scroll_status_rects(win.state().size);

    let mut status = status;
    status.state_mut().set_bounds(r.status);
    let status_id = win.insert_child(status);

    let bar_id = win.insert_child(Box::new(ScrollBar::new(r.bar)));
    let body_id = win.insert_child(body(r.body, Some(bar_id)));

    (
        win,
        ScrollStatusIds {
            status: status_id,
            body: body_id,
            bar: bar_id,
        },
    )
}

fn apply_scroll_status_layout(win: &mut Window, ids: &ScrollStatusIds) {
    let r = scroll_status_rects(win.state().size);
    for (id, rect) in [(ids.status, r.status), (ids.bar, r.bar), (ids.body, r.body)] {
        if let Some(view) = win.child_mut(id) {
            view.change_bounds(rect);
        }
    }
}

pub struct ScrollStatusWindow {
    window: Window,
    ids: ScrollStatusIds,
}

impl ScrollStatusWindow {
    pub fn new(
        bounds: Rect,
        title: impl Into<String>,
        number: i16,
        status: Box<dyn View>,
        body: impl FnOnce(Rect, Option<ViewId>) -> Box<dyn View>,
    ) -> Self {
        let (window, ids) = build_scroll_status_window(bounds, title, number, status, body);
        Self { window, ids }
    }

    pub fn ids(&self) -> &ScrollStatusIds {
        &self.ids
    }

    pub fn window_mut(&mut self) -> &mut Window {
        &mut self.window
    }
}

#[delegate(to = window)]
impl View for ScrollStatusWindow {
    fn state(&self) -> &tv::ViewState {
        self.window.state()
    }
    fn state_mut(&mut self) -> &mut tv::ViewState {
        self.window.state_mut()
    }

    fn draw(&mut self, ctx: &mut DrawCtx) {
        self.window.draw(ctx);
    }

    fn change_bounds(&mut self, bounds: Rect) {
        self.window.change_bounds(bounds);
        apply_scroll_status_layout(&mut self.window, &self.ids);
    }

    fn handle_event(&mut self, ev: &mut tv::event::Event, ctx: &mut tv::Context) {
        let before = self.window.state().size;
        // Windows ▸ Full screen: this wrapper owns the inner window.
        let full = crate::app::take_fullscreen(ev, &mut self.window, ctx);
        if !full {
            self.window.handle_event(ev, ctx);
        }
        // Zoom / fullscreen re-bound the inner window's group directly
        // (`Window::maximize` -> `locate` -> `group.change_bounds`), which never
        // reaches this wrapper's `change_bounds`; reconcile so the status/body/
        // bar follow the new client area (also after a border-only flip).
        if full || self.window.state().size != before {
            apply_scroll_status_layout(&mut self.window, &self.ids);
            self.window.on_bounds_changed(ctx);
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
