#![forbid(unsafe_code)]
//! Screen-level regression tests for the pane layout (round-2 review):
//! 1) children must never overlap the window frame (the reported
//!    border-overwrite), 2) the body must refill the client area after a
//!    zoom/Size-Move resize (the reported "lower half blank").
mod common;

use common::{SharedClock, screen_text};
use std::cell::RefCell;
use std::rc::Rc;
use tv::event::{Key, KeyModifiers};
use tv::widgets::{SelectionMode, Table};
use tv::{Desktop, DrawCtx, HeadlessBackend, HeadlessHandle, Program, Rect, Role, View, ViewState};
use tvdocker::app;
use tvdocker::widgets::resource_table::{RowModel, TableRow, to_spec};
use tvdocker::widgets::{Align, Column, FilterBar, PaneWindow};

/// Stands in for the probes' info line (the real ones draw stats text).
struct InfoStub {
    st: ViewState,
}
impl View for InfoStub {
    fn state(&self) -> &ViewState {
        &self.st
    }
    fn state_mut(&mut self) -> &mut ViewState {
        &mut self.st
    }
    fn draw(&mut self, ctx: &mut DrawCtx) {
        ctx.put_str(0, 0, "INFO", ctx.style(Role::Info));
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn rows(n: usize) -> Vec<TableRow> {
    (0..n)
        .map(|i| {
            TableRow::new(
                format!("id-{i:02}"),
                vec![format!("id-{i:02}"), "name".into()],
            )
        })
        .collect()
}

/// The probe window, built exactly the way the examples now build it.
fn probe_pane() -> PaneWindow {
    let sink = Rc::new(RefCell::new(String::new()));
    PaneWindow::new(
        Rect::new(2, 1, 78, 23),
        "table_probe",
        1,
        Box::new(InfoStub {
            st: ViewState::new(Rect::new(0, 0, 1, 1)),
        }),
        Box::new(FilterBar::new(
            Rect::new(0, 0, 1, 1),
            sink,
            "type to filter",
        )),
        |rect, bar| {
            let columns = [
                Column::new("ID", 12, Align::Left),
                Column::new("NAME", 30, Align::Left),
                Column::new("STATE", 12, Align::Left),
            ];
            let mut table = Table::new(rect, columns.iter().map(to_spec).collect(), bar)
                .with_selection_mode(SelectionMode::Multi)
                .with_model(Box::new(RowModel::new(rows(50))));
            table.state_mut().options.selectable = true;
            Box::new(table)
        },
    )
}

fn start() -> (Program, HeadlessHandle) {
    let (backend, screen) = HeadlessBackend::new(80, 25);
    let (shared, _clock) = SharedClock::new(0);
    let program = app::program_with(Box::new(backend), Box::new(shared), |r| {
        let mut r = r;
        r.a.y += 1;
        r.b.y -= 1;
        let mut desktop = Desktop::new(r, |br| Some(tv::Desktop::init_background(br)));
        desktop.insert_view(Box::new(probe_pane()));
        Some(Box::new(desktop))
    });
    (program, screen)
}

#[test]
fn children_never_touch_the_window_frame() {
    let (mut program, screen) = start();
    for _ in 0..3 {
        program.pump_once();
    }
    let text = screen_text(&screen);
    let line: Vec<&str> = text.lines().collect();

    // Top frame row (screen row 2) holds the title and NO table content.
    assert!(
        line[2].contains("table_probe"),
        "title on top frame: {}",
        line[2]
    );
    assert!(
        !line[2].contains("ID"),
        "header must not overwrite the top frame: {}",
        line[2]
    );

    // Client rows: info visible (old zero-height bug), filter, header inside.
    assert!(line[3].contains("INFO"), "info row visible: {}", line[3]);
    assert!(
        line[4].contains("type to filter"),
        "filter row: {}",
        line[4]
    );
    assert!(
        line[5].contains("ID"),
        "header inside the client: {}",
        line[5]
    );

    // Bottom frame row (screen row 23): frame intact, NO body content
    // (the reported border overwrite — old rect y2=22 covered it).
    assert!(line[23].contains('└'), "bottom frame intact: {}", line[23]);
    assert!(
        !line[23].contains("id-"),
        "body must not overwrite the bottom frame: {}",
        line[23]
    );
}

#[test]
fn body_refills_the_client_after_zoom() {
    let (mut program, screen) = start();
    // 10 pumps: the status line's COMMAND_SET_CHANGED cache must settle
    // before F5 transforms (observed: 3 pumps is too few with children).
    for _ in 0..10 {
        program.pump_once();
    }
    screen.push_key(Key::F(5), KeyModifiers::default());
    for _ in 0..3 {
        program.pump_once();
    }
    let text = screen_text(&screen);
    let line: Vec<&str> = text.lines().collect();

    assert!(
        line[1].contains("table_probe"),
        "zoom actually happened: {}",
        line[1]
    );
    // The reported symptom: after resize the lower client stays blank.
    // With draw-time relayout the body's last row is screen row 22.
    assert!(
        line[22].contains("id-"),
        "body refilled the grown client (row 22): {}",
        line[22]
    );
    assert!(
        line[23].contains('└'),
        "bottom frame still intact after zoom: {}",
        line[23]
    );
    assert!(
        !line[23].contains("id-"),
        "no body content on the frame: {}",
        line[23]
    );
}
