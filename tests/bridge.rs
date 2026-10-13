#![forbid(unsafe_code)]
//! Bridge integration test : messages drain on the bridge timer
//! with zero user input, and the refreshed state reaches the screen.
mod common;

use common::{SharedClock, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::mpsc;
use tv::event::Event;
use tv::{Context, DrawCtx, HeadlessBackend, Program, Rect, Role, Theme, View, ViewState};
use tvdocker::bridge::{Bridge, REFRESH};

/// Minimal view that renders shared state; exists to observe the screen.
struct LabelView {
    st: ViewState,
    shared: Rc<RefCell<u32>>,
}

impl View for LabelView {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        let total = *self.shared.borrow();
        ctx.put_str(
            1,
            1,
            &format!("total: {total}"),
            ctx.style(Role::StaticText),
        );
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[test]
fn bridge_drains_all_queued_messages_on_its_timer_and_the_screen_refreshes() {
    let (backend, screen) = HeadlessBackend::new(30, 6);
    let (shared_clock, clock) = SharedClock::new(0);
    let shared = Rc::new(RefCell::new(0u32));
    let (tx, rx) = mpsc::channel::<u32>(64);

    let mut program = Program::new(
        Box::new(backend),
        Box::new(shared_clock),
        Theme::classic_blue(),
        {
            let shared = Rc::clone(&shared);
            move |r| {
                let mut r = r;
                r.a.y += 1;
                r.b.y -= 1;
                let mut desktop = tv::Desktop::new(r, |br| Some(tv::Desktop::init_background(br)));
                desktop.insert_view(Box::new(LabelView {
                    st: ViewState::new(Rect::new(1, 1, 28, 5)),
                    shared: Rc::clone(&shared),
                }));
                desktop.insert_view(Box::new(Bridge::new(
                    Rc::clone(&shared),
                    rx,
                    |state: &mut u32, delta: u32| *state += delta,
                )));
                Some(Box::new(desktop))
            }
        },
        |_| None,
        |_| None,
    );

    // Arm the bridge deterministically: a broadcast reaches every child.
    screen.push_event(Event::Broadcast {
        command: REFRESH,
        source: None,
    });
    for _ in 0..2 {
        program.pump_once();
    }

    tx.try_send(1).unwrap();
    tx.try_send(2).unwrap();

    // Advance past the 50 ms timer; pumps collect it and drain.
    clock.advance(60);
    for _ in 0..4 {
        program.pump_once();
    }

    assert_eq!(*shared.borrow(), 3, "both messages applied once");
    let text = screen_text(&screen);
    assert!(
        text.contains("total: 3"),
        "screen shows refreshed state: {text:?}"
    );

    // No spurious re-application: advancing more must not change the total.
    clock.advance(60);
    for _ in 0..3 {
        program.pump_once();
    }
    assert_eq!(*shared.borrow(), 3, "empty drains do not double-count");
}

#[test]
fn bridge_arms_from_startup_events_without_injection() {
    // Lifecycle invariant (Task 3 design note): a production-style desktop
    // — selectable window + bridge, NO synthetic events pushed by the test
    // — must arm its timer from startup broadcasts alone and drain.
    let (backend, screen) = HeadlessBackend::new(30, 6);
    let (shared_clock, clock) = SharedClock::new(0);
    let shared = Rc::new(RefCell::new(0u32));
    let (tx, rx) = mpsc::channel::<u32>(64);

    let mut program = Program::new(
        Box::new(backend),
        Box::new(shared_clock),
        Theme::classic_blue(),
        {
            let shared = Rc::clone(&shared);
            move |r| {
                let mut r = r;
                r.a.y += 1;
                r.b.y -= 1;
                let mut desktop = tv::Desktop::new(r, |br| Some(tv::Desktop::init_background(br)));
                let mut win = tv::Window::new(Rect::new(1, 1, 28, 5), Some("prod".into()), 1);
                win.state_mut().options.tileable = true;
                win.insert_child(Box::new(LabelView {
                    st: ViewState::new(Rect::new(1, 1, 26, 3)),
                    shared: Rc::clone(&shared),
                }));
                desktop.insert_view(Box::new(win));
                desktop.insert_view(Box::new(Bridge::new(
                    shared,
                    rx,
                    |s: &mut u32, delta: u32| *s += delta,
                )));
                Some(Box::new(desktop))
            }
        },
        |_| None,
        |_| None,
    );

    // Pump startup only — nothing is pushed by this test.
    for _ in 0..3 {
        program.pump_once();
    }
    tx.try_send(7).unwrap();
    clock.advance(60);
    for _ in 0..4 {
        program.pump_once();
    }
    assert_eq!(*shared.borrow(), 7, "timer armed from startup events alone");
    assert!(
        screen_text(&screen).contains("total: 7"),
        "screen refreshed"
    );
}

/// Modal that closes itself once it has OBSERVED the drained state via the
/// REFRESH broadcast — forces the modal loop to keep spinning (idle passes)
/// until the bridge has actually applied the message mid-modal.
struct GateModal {
    st: ViewState,
    shared: Rc<RefCell<u32>>,
}

impl View for GateModal {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        let style = ctx.style(Role::Info);
        ctx.fill(self.st.get_extent(), ' ', style);
        ctx.put_str(2, 1, "modal gate", style);
    }
    fn handle_event(&mut self, ev: &mut Event, ctx: &mut Context) {
        if let Event::Broadcast { command, .. } = &*ev
            && *command == REFRESH
            && *self.shared.borrow() >= 42
        {
            // Dialog pattern (dialog.rs:322): end the modal from the view.
            ctx.end_modal(tv::Command::OK);
            ev.clear();
        }
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[test]
fn bridge_drains_while_a_modal_is_open() {
    // §13 gate: updates flow while a modal dialog is open. The clock is
    // pre-advanced past the 50 ms deadline; the modal loop's IDLE passes
    // collect the expired timer, the Timer event (broadcast-class) reaches
    // the bridge mid-modal, and the REFRESH it emits lets GateModal close.
    // If the drain happened only after the modal, this would hang/under-count.
    let (backend, screen) = HeadlessBackend::new(40, 10);
    let (shared_clock, clock) = SharedClock::new(0);
    let shared = Rc::new(RefCell::new(0u32));
    let (tx, rx) = mpsc::channel::<u32>(64);

    let mut program = Program::new(
        Box::new(backend),
        Box::new(shared_clock),
        Theme::classic_blue(),
        {
            let shared = Rc::clone(&shared);
            move |r| {
                let mut r = r;
                r.a.y += 1;
                r.b.y -= 1;
                let mut desktop = tv::Desktop::new(r, |br| Some(tv::Desktop::init_background(br)));
                desktop.insert_view(Box::new(LabelView {
                    st: ViewState::new(Rect::new(1, 1, 38, 5)),
                    shared: Rc::clone(&shared),
                }));
                desktop.insert_view(Box::new(Bridge::new(
                    Rc::clone(&shared),
                    rx,
                    |state: &mut u32, delta: u32| *state += delta,
                )));
                Some(Box::new(desktop))
            }
        },
        |_| None,
        |_| None,
    );

    // Arm + let the startup event flow settle (idle passes collect nothing
    // yet — the deadline has not been reached).
    screen.push_event(Event::Broadcast {
        command: REFRESH,
        source: None,
    });
    for _ in 0..2 {
        program.pump_once();
    }
    tx.try_send(42).unwrap();
    clock.advance(60); // timer deadline reached; collection happens inside

    let modal = GateModal {
        st: ViewState::new(Rect::new(5, 2, 35, 6)),
        shared: Rc::clone(&shared),
    };
    program.exec_view_with(Box::new(modal), |_, _| ());

    assert_eq!(
        *shared.borrow(),
        42,
        "message applied while the modal loop was running"
    );
    // The modal loop's last render still has the dialog on top; one pump
    // after removal repaints the desktop from the drained state.
    program.pump_once();
    assert!(
        screen_text(&screen).contains("total: 42"),
        "screen refreshed from the drained state"
    );
}
