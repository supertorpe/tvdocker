//! The Shell/PTY window: the probe's wiring as
//! a window — `ScrollStatusWindow` (status row + terminal with right scroll bar)
//! plus the `Bridge` that drains `PtyMsg` into `PtyState`. Closing the window
//! drops the receiver, which tears the worker's exec down by itself.

use crate::bridge::Bridge;
use crate::widgets::ScrollStatusWindow;
use crate::widgets::pty::{PtyMsg, PtyPhase, PtyState, PtyView, SCROLLBACK_LINES, StatusView};
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::{mpsc, watch};
use tv::{Rect, View, ViewState, delegate};

/// Window-menu number for Shell instances.
const WINDOW_NUMBER: i16 = 9;

/// The Shell window: the `ScrollStatusWindow` plus the slot's open flag
/// — cleared on drop so the slot can reopen the shell (the
/// `DetailsWindow`/`LogsWindow` pattern). Without it the desktop keeps
/// believing a shell is open and "focuses" the DEAD window id, which
/// silently no-ops: F4 would do nothing at all.
pub struct ShellWindow {
    win: ScrollStatusWindow,
    open: Rc<RefCell<bool>>,
}

/// Build the shell window around channels the DESKTOP created (the
/// worker owns the other halves — decisions #17/#20). The initial
/// parser geometry mirrors `examples/pty_probe.rs:53-58`; the first
/// `PtyView::draw` resizes it to the real client rect anyway. Returns
/// the shared state (tests assert phase transitions through it).
pub fn build(
    bounds: Rect,
    label: &str,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    resize_tx: watch::Sender<(u16, u16)>,
    out_rx: mpsc::Receiver<PtyMsg>,
    open: Rc<RefCell<bool>>,
) -> (ShellWindow, Rc<RefCell<PtyState>>) {
    let state = Rc::new(RefCell::new(PtyState {
        parser: vt100::Parser::new(24, 80, SCROLLBACK_LINES),
        note: "starting…".to_string(),
        phase: PtyPhase::Running,
        size: (80, 24),
    }));
    let for_status = Rc::clone(&state);
    let for_view = Rc::clone(&state);
    let for_bridge = Rc::clone(&state);
    let mut win = ScrollStatusWindow::new(
        bounds,
        format!("shell @ {label}"),
        WINDOW_NUMBER,
        Box::new(StatusView::new(Rect::new(0, 0, 1, 1), for_status)),
        move |rect, bar| {
            let mut view = PtyView::new(rect, for_view, input_tx, resize_tx);
            // Link the scrollbar the scroll+status layout inserted. The body
            // follows the live bottom; Shift+↑↓/PgUp/PgDn/Home/End, the wheel
            // and this bar drive the scrollback, while the BARE navigation
            // keys keep going to the shell as ANSI input.
            view.set_v_scroll_bar(bar);
            Box::new(view)
        },
    );
    // The drain: a zero-area child INSIDE this window (auto-removed
    // with it — unlike the probe's desktop-level sibling, which would
    // accumulate one per shell). Non-positional events (Timer) reach it
    // through the window's group fan-out, the same way the probe's
    // desktop-level bridge receives them.
    win.window_mut()
        .insert_child(Box::new(Bridge::new(for_bridge, out_rx, PtyState::apply)));
    (ShellWindow { win, open }, state)
}

impl Drop for ShellWindow {
    /// Clear the slot's open flag so closing the window lets F4 reopen
    /// the shell config (mirrors `DetailsWindow`; the stale-ViewId focus
    /// path would otherwise silently no-op).
    fn drop(&mut self) {
        *self.open.borrow_mut() = false;
    }
}

#[delegate(to = win)]
impl View for ShellWindow {
    fn state(&self) -> &ViewState {
        self.win.state()
    }

    fn state_mut(&mut self) -> &mut ViewState {
        self.win.state_mut()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{render_view, text};

    #[test]
    fn shell_window_shows_title_starting_note_and_phase_transitions() {
        let (out_tx, out_rx) = mpsc::channel::<PtyMsg>(4);
        let (input_tx, _input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (resize_tx, _resize_rx) = watch::channel((80u16, 24u16));
        let open = Rc::new(RefCell::new(true));
        let (mut win, state) = build(
            Rect::new(2, 1, 78, 23),
            "web",
            input_tx,
            resize_tx,
            out_rx,
            Rc::clone(&open),
        );
        let out = text(&render_view(76, 23, &mut win));
        assert!(out.contains("shell @ web"), "title (probe format): {out:?}");
        assert!(
            out.contains("starting…"),
            "status row while booting: {out:?}"
        );

        // The worker reports the exit — the STATUS ROW (not the
        // terminal) carries it (widgets/pty.rs contract).
        state
            .borrow_mut()
            .apply(PtyMsg::Phase(PtyPhase::Exited { code: Some(2) }));
        let out = text(&render_view(76, 23, &mut win));
        assert!(
            out.contains("[process exited with code 2]"),
            "phase text: {out:?}"
        );
        drop(out_tx); // complete the pair; nothing streams in this test
    }

    /// Closing the window must clear the slot's open flag — otherwise
    /// the desktop still believes a shell is open and F4 "focuses" the
    /// DEAD window id (a silent no-op): the shell could never be
    /// reopened. Mirrors `DetailsWindow`/`LogsWindow`.
    #[test]
    fn drop_clears_the_slot_open_flag() {
        let (out_tx, out_rx) = mpsc::channel::<PtyMsg>(4);
        let (input_tx, _input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (resize_tx, _resize_rx) = watch::channel((80u16, 24u16));
        let open = Rc::new(RefCell::new(true));
        let (win, _state) = build(
            Rect::new(2, 1, 78, 23),
            "web",
            input_tx,
            resize_tx,
            out_rx,
            Rc::clone(&open),
        );
        assert!(*open.borrow(), "open while the window lives");
        drop(win);
        assert!(
            !*open.borrow(),
            "close ⇒ the slot can reopen the shell (F4 must work again)"
        );
        drop(out_tx);
    }
}
