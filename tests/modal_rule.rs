#![forbid(unsafe_code)]
//! tvision-rs 0.17's modal rule: *a modal alone receives keys and commands*,
//! so a global shortcut that would otherwise act on the whole app (Alt-X quit,
//! Alt-F the menu bar) is **dropped** while a box is up. The 0.16 behaviour —
//! the status line kept the behind-the-modal help context, so the app-level
//! command ran — is gone; this pins the new rule end to end through the real
//! pump.
//!
//! The drive rides the `resource_actions.rs` idiom: `request_exec_view` stashes
//! the dialog in `Program::pending_modal`, which only `run_app`'s private
//! `pump_and_drive` executes, so the whole key stream is pre-queued and ended
//! with [`close_modal_and_quit`]. An open modal swallows *its* keys, which is
//! what proves it existed.
mod common;

use common::{AppHarness, app_start, close_modal_and_quit, container, screen_text};
use tv::Command;
use tv::event::{Event, Key, KeyModifiers};
use tvdocker::app;
use tvdocker::core::{AppMsg, TaskReq};
use tvdocker::views::dialogs::ShellForm;

fn alt() -> KeyModifiers {
    KeyModifiers {
        alt: true,
        ..KeyModifiers::default()
    }
}

fn plain() -> KeyModifiers {
    KeyModifiers::default()
}

/// Alt-X is the global quit. With the F4 shell-config modal up it must be
/// swallowed whole: the app keeps running, the OK still submits, and — because
/// the status line captured Alt-X as the `QUIT` *command* rather than letting
/// the focused `InputLine` see the keystroke — the field text is untouched.
///
/// The discriminator is the `StartShell` task: had Alt-X leaked and quit, the
/// drive would have ended before the OK and the channel would be empty; had
/// the key been typed as text, the submitted `shell` would carry an `x`.
#[test]
fn an_open_modal_swallows_the_global_quit_shortcut() {
    let mut h: AppHarness = app_start(|core| {
        core.borrow_mut()
            .apply(AppMsg::Containers(vec![container("id-alpha", "webalpha")]));
    });

    // Containers' help context binds F4 to the shell command; the modal opens
    // at the end of `run_app`'s first iteration, so the keys below land on it.
    h.screen.push_key(Key::F(4), plain());
    // The global quit — must be swallowed by the modal above.
    h.screen.push_key(Key::Char('x'), alt());
    // The same command the "live Enter" reaches the modal with; it is handled,
    // so the modal closes and the form is submitted (and gathered).
    h.screen.push_event(Event::Command(Command::OK));
    close_modal_and_quit(&h.screen);
    app::run_program(h.program, None);

    match h.task_rx.try_recv() {
        Ok(TaskReq::StartShell { id, shell, .. }) => {
            assert_eq!(id, "id-alpha", "the pending container");
            assert_eq!(
                shell,
                ShellForm::prefill("webalpha", "img").shell,
                "Alt-X was swallowed, not typed into the field"
            );
        }
        _ => panic!("the modal must swallow Alt-X and still submit a StartShell"),
    }
    assert!(
        screen_text(&h.screen).contains("shell @ webalpha"),
        "the PTY window opened ⇒ the app survived Alt-X: {}",
        screen_text(&h.screen)
    );
}
