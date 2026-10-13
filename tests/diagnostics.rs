#![forbid(unsafe_code)]
//! *Diagnose…* reaches the worker and the ×N window
//! The harness never runs the worker, so a test
//! asserts the *request*, not a response — no network or Docker daemon.
mod common;

use common::{container, pump, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Event, Key, KeyModifiers, MouseButtons, MouseEvent};
use tv::{Command, HeadlessHandle, Point, Program};
use tvdocker::app;
use tvdocker::core::{AppCore, AppMsg, TaskReq};
use tvdocker::domain::config::{LlmConfig, TvdockerConfig};

type TaskRx = tokio::sync::mpsc::UnboundedReceiver<TaskReq>;

fn start() -> (Program, HeadlessHandle, Rc<RefCell<AppCore>>, TaskRx) {
    let h = common::app_start(|core| {
        core.borrow_mut()
            .apply(AppMsg::Containers(vec![container("id-a", "webalpha")]));
    });
    (h.program, h.screen, h.core, h.task_rx)
}

fn right_click(screen: &HeadlessHandle, x: i32, y: i32) {
    screen.push_event(Event::MouseDown(MouseEvent {
        position: Point::new(x, y),
        buttons: MouseButtons {
            right: true,
            ..MouseButtons::default()
        },
        ..MouseEvent::default()
    }));
}

/// Right-click row 0 of the Containers table and pick *Diag~n~ose…*.
/// Assumes the Containers window is the one under the cursor — call
/// [`cycle_focus`] first when another window covers the row.
fn menu_diagnose(program: &mut Program, screen: &HeadlessHandle) {
    right_click(screen, 12, 6);
    // The popup needs several cycles to paint; a second invocation (over a
    // freshly raised window) needs more than the first.
    pump(program, 10);
    screen.push_key(Key::Char('n'), KeyModifiers::default());
    pump(program, 6);
}

/// The focused Diagnostics window's frame position `(row, column of the
/// `╔` corner)`. The cascade gives every NEW window a fresh origin, so an
/// unchanged position proves the same window is on top.
fn diagnostics_frame(text: &str) -> Option<(usize, usize)> {
    text.lines().enumerate().find_map(|(y, line)| {
        line.contains("Diagnostics — webalpha")
            .then(|| line.find('╔').map(|x| (y, x)))
            .flatten()
    })
}

/// Cycle top-level focus (F6 = Next, pre-routed to `Command::NEXT`).
/// A newly opened window covers the row, so a second menu invocation
/// needs the Containers window focused again.
fn cycle_focus(program: &mut Program, screen: &HeadlessHandle) {
    screen.push_event(Event::Command(Command::NEXT));
    pump(program, 5);
}

/// Take the next `StartDiagnostics`, or fail with a name for the phase.
/// (`TaskReq` is not `Debug`, so the message can't print the value.)
fn next_run(task_rx: &mut TaskRx, phase: &str) -> (String, u64, LlmConfig) {
    match task_rx.try_recv() {
        Ok(TaskReq::StartDiagnostics { id, run, config }) => (id, run, config),
        _ => panic!("expected a StartDiagnostics request at: {phase}"),
    }
}

/// Review Focus #1 + #2: the menu item reaches the desktop, and ONE
/// selection both opens the window and requests the run (§9).
#[test]
fn diagnose_opens_the_window_and_requests_a_run() {
    let (mut program, screen, _core, mut task_rx) = start();
    menu_diagnose(&mut program, &screen);

    let text = screen_text(&screen);
    assert!(text.contains("Diagnostics"), "the window opened: {text:?}");

    let (id, run, _config) = next_run(&mut task_rx, "first diagnose");
    assert_eq!(id, "id-a", "the clicked container");
    assert!(run > 0, "a run id is assigned");
}

/// Review Focus #3: a re-diagnose restarts the run in the SAME window —
/// no second window, no missed run .
#[test]
fn re_diagnose_reuses_the_window_and_bumps_the_run() {
    let (mut program, screen, _core, mut task_rx) = start();
    menu_diagnose(&mut program, &screen);
    let (id, first, _) = next_run(&mut task_rx, "first diagnose");
    assert_eq!(id, "id-a", "the clicked container");
    let frame = diagnostics_frame(&screen_text(&screen))
        .expect("the Diagnostics window is on screen after the first run");
    // The new window now covers the table: cycle back to Containers.
    cycle_focus(&mut program, &screen);
    menu_diagnose(&mut program, &screen);
    let (_id, second, _) = next_run(&mut task_rx, "second diagnose");
    assert!(
        second > first,
        "the re-run bumped the sequence: {first} → {second}"
    );

    // Still exactly ONE Diagnostics window: a second one would be built at
    // the NEXT cascade origin, so the focused frame's position would move.
    let after = diagnostics_frame(&screen_text(&screen));
    assert_eq!(
        after,
        Some(frame),
        "the run reused the open window instead of building a second one"
    );
}

/// `Tab` switches pane, and the scroll keys then reach the OTHER
/// pane. This is the evidence behind phase 4's status-def ruling — `Tab` is
/// deliberately NOT bound by the status line, because the framework's `Group`
/// handles it itself (a `key_item` would steal it from the focused pane).
#[test]
fn tab_switches_which_pane_the_scroll_keys_reach() {
    let (mut program, screen, core, mut task_rx) = start();
    menu_diagnose(&mut program, &screen);
    let (id, run, _config) = next_run(&mut task_rx, "first diagnose");

    // More lines than fit, so scrolling is visible. Seeded AFTER the run
    // starts (a run resets both texts) and BEFORE the window refreshes.
    let analysis: String = (0..40)
        .map(|i| format!("a{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let playbook: String = (0..40)
        .map(|i| format!("p{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    core.borrow_mut().apply(AppMsg::DiagnosticsAnalysis {
        id: id.clone(),
        run,
        text: analysis,
    });
    core.borrow_mut().apply(AppMsg::DiagnosticsPlaybook {
        id,
        run,
        text: playbook,
    });

    // Before the first Tab the ANALYSIS pane holds focus (phase 3's
    // `request_initial_focus`, now proven end-to-end), so the first Down trio
    // must scroll IT. The window also refreshes from the core on this first
    // key, which is what makes the seeded lines appear.
    for _ in 0..3 {
        screen.push_key(Key::Down, KeyModifiers::default());
        pump(&mut program, 2);
    }
    let text = screen_text(&screen);
    assert!(
        text.contains("a03"),
        "the ANALYSIS scrolled first: {text:?}"
    );
    assert!(
        !text.contains("a00"),
        "its first line scrolled away: {text:?}"
    );
    assert!(text.contains("p00"), "the playbook was untouched: {text:?}");
    assert!(
        !text.contains("Alt-X Exit"),
        "the Diagnostics status def is selected (it hides Alt-X): {text:?}"
    );

    // Tab moves to the playbook pane; the keys now reach IT.
    screen.push_key(Key::Tab, KeyModifiers::default());
    pump(&mut program, 3);
    for _ in 0..3 {
        screen.push_key(Key::Down, KeyModifiers::default());
        pump(&mut program, 2);
    }
    let text = screen_text(&screen);
    assert!(text.contains("p03"), "the playbook scrolled: {text:?}");
    assert!(
        !text.contains("p00"),
        "its first line scrolled away: {text:?}"
    );
    assert!(
        text.contains("a03"),
        "the ANALYSIS kept its offset: {text:?}"
    );
}

/// Review Focus #4: `[llm]` is re-read for EVERY run — a
/// cached config would defeat it.
#[test]
fn each_run_re_reads_the_llm_config() {
    let (mut program, screen, _core, mut task_rx) = start();
    menu_diagnose(&mut program, &screen);
    let (_id, _run, first) = next_run(&mut task_rx, "first diagnose");
    assert_eq!(first.provider, "ollama", "the default [llm] provider");

    // Change `[llm]` in the ISOLATED config, then run again.
    let path = common::isolate_config();
    let changed = TvdockerConfig {
        llm: Some(LlmConfig {
            provider: "second-provider".into(),
            ..LlmConfig::default()
        }),
        ..TvdockerConfig::default()
    };
    changed.save().expect("the isolated config is writable");

    cycle_focus(&mut program, &screen);
    menu_diagnose(&mut program, &screen);
    let (_id, _run, second) = next_run(&mut task_rx, "second diagnose");
    assert_eq!(
        second.provider, "second-provider",
        "the re-read [llm] provider reached the worker"
    );

    // Leave the isolated file clean for the other tests in this binary.
    std::fs::remove_file(&path).ok();
}

/// Review Focus #5: a run failure never opens a modal (§10) — the error
/// text lands in the UI (status latch + window phase), and the window
/// stays open.
#[test]
fn a_failed_run_reports_in_the_ui_not_a_dialog() {
    let (mut program, screen, core, mut task_rx) = start();
    menu_diagnose(&mut program, &screen);
    let (id, run, _config) = next_run(&mut task_rx, "first diagnose");

    // Enough content in both panes that a scroll is visible: this is what
    // proves no modal is swallowing keys (the repo's modal pattern — an open
    // box eats its keys, `tests/resource_actions.rs`).
    let analysis: String = (0..40)
        .map(|i| format!("a{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let playbook: String = (0..40)
        .map(|i| format!("p{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    core.borrow_mut().apply(AppMsg::DiagnosticsAnalysis {
        id: id.clone(),
        run,
        text: analysis,
    });
    core.borrow_mut().apply(AppMsg::DiagnosticsPlaybook {
        id: id.clone(),
        run,
        text: playbook,
    });

    // The worker reports a failure; §10 keeps it out of a message box.
    core.borrow_mut().apply(AppMsg::DiagnosticsError {
        id,
        run,
        message: "collect id-a: no such container".into(),
    });

    // Tab refreshes the window AND switches pane; the following Down trio
    // scrolls that pane. Run to completion so any pending modal would be
    // executed by the real pump, then check it did NOT eat those keys.
    screen.push_key(Key::Tab, KeyModifiers::default());
    for _ in 0..3 {
        screen.push_key(Key::Down, KeyModifiers::default());
    }
    screen.push_event(Event::Command(Command::QUIT));
    screen.push_event(Event::Command(Command::QUIT));
    app::run_program(program, None);

    let text = screen_text(&screen);
    assert!(
        text.contains("no such container"),
        "the failure is shown in the UI: {text:?}"
    );
    assert!(
        text.contains("Diagnostics — webalpha"),
        "the window is still open: {text:?}"
    );
    assert!(
        text.contains("p03") && !text.contains("p00"),
        "the keys reached the pane, so no modal was open to swallow them: {text:?}"
    );
}
