//! The async bridge : tokio tasks publish `M` over an mpsc
//! channel; a zero-area view with a repeating 50 ms timer drains it on the
//! UI thread and broadcasts [`REFRESH`].
//!
//! Mechanism choice vs the spec's original "idle hook" wording (§8 amended):
//! tvision-rs documents two drain patterns — the program-level
//! `Program::set_on_idle` hook, and this view-owned zero-area periodic timer
//! (`Context::set_timer` + `Context::broadcast`, which the framework docs
//! explicitly recommend for view-owned drains). We use the timer: the drain
//! lives in a view with `Context` access, symmetric with the probes. The run
//! loop polls with `min(20 ms frame tick, time-until-next-timer)` and
//! collects expired timers on idle passes, so the bridge advances with zero
//! user input and while modal dialogs are open (modals idle-spin until input;
//! they filter positional events, not timers or broadcasts). The timer's
//! cost — first-event arming — is tested
//! (`bridge_arms_from_startup_events_without_injection`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use tokio::sync::mpsc::Receiver;
use tv::{Command, Context, DrawCtx, Event, Rect, TimerId, View, ViewState};

/// Views repaint from shared state after each drain .
pub const REFRESH: Command = Command::custom("tvdocker.refresh");

/// Drain period. Also the structural update throttle (NFRs).
const DRAIN_PERIOD: Duration = Duration::from_millis(50);

/// Zero-area invisible view that drains `rx` into shared state `S`.
///
/// `apply` is a plain function pointer, so reducers fit without generic
/// closures. The channel is `tokio::sync::mpsc`
/// (gate: *tokio + mpsc + timer-driven drain*); `try_recv` needs
/// no runtime, so draining stays synchronous on the UI thread.
///
/// The channel is **bounded** (`APP_CHANNEL_CAP` = 1024,
/// drop-new at the producer): a flood loses intermediate messages while the
/// next poll supersedes them; the bridge drains everything kept in one tick.
pub struct Bridge<S, M> {
    st: ViewState,
    state: Rc<RefCell<S>>,
    rx: Receiver<M>,
    apply: fn(&mut S, M),
    timer: Option<TimerId>,
}

impl<S, M> Bridge<S, M> {
    /// `state` is the same handle views read; `rx` receives messages from
    /// background tasks .
    pub fn new(state: Rc<RefCell<S>>, rx: Receiver<M>, apply: fn(&mut S, M)) -> Self {
        // Zero-area: never painted, still fully routable (timer target).
        Self {
            st: ViewState::new(Rect::new(0, 0, 0, 0)),
            state,
            rx,
            apply,
            timer: None,
        }
    }

    fn arm(&mut self, ctx: &mut Context) {
        if self.timer.is_none() {
            self.timer = Some(ctx.set_timer(DRAIN_PERIOD, Some(DRAIN_PERIOD)));
        }
    }

    /// Apply everything queued; return how many messages were applied.
    fn drain(&mut self) -> usize {
        let mut applied = 0;
        // `Empty` or `Disconnected` ends the loop; `Disconnected` only
        // surfaces once the queue is exhausted — nothing is lost.
        while let Ok(msg) = self.rx.try_recv() {
            (self.apply)(&mut self.state.borrow_mut(), msg);
            applied += 1;
        }
        applied
    }
}

impl<S: 'static, M: 'static> View for Bridge<S, M> {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }

    fn draw(&mut self, _ctx: &mut DrawCtx) {
        // Zero-area: nothing to paint.
    }

    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        // Any event proves the app is alive → start (once) the timer.
        // In production the first event is the startup RECEIVED_FOCUS
        // broadcast from the desktop's selectable window — proven by
        // `bridge_arms_from_startup_events_without_injection`.
        self.arm(ctx);
        if let Event::Timer(id) = ev
            && Some(*id) == self.timer
        {
            if self.drain() > 0 {
                ctx.broadcast(REFRESH, None);
            }
            ev.clear();
        }
        // Everything else (incl. broadcasts) is not ours — the default
        // behavior routes/bubbles it.
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
