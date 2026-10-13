//! The Events window : the live event stream over
//! `AppCore::events`, mapped NEWEST-FIRST (— the Table has
//! no follow-tail; clicking the TIME header ascending gives
//! chronological order on demand).

use crate::core::AppCore;
use crate::views::resource::{self, ResourceSpec, ResourceWindow};
use crate::widgets::{Align, Column, TableRow};
use std::cell::RefCell;
use std::rc::Rc;
use tv::{HelpCtx, Rect};

/// Help context while the Events window is focused .
pub const HELP_CTX: HelpCtx = HelpCtx::custom("tvdocker.events");

/// Columns (new — no oh-my-docker equivalent row layout;
/// no icons/colors, the theme has no per-kind roles).
pub fn columns() -> Vec<Column> {
    vec![
        Column::new("TIME", 9, Align::Left),
        Column::new("TYPE", 10, Align::Left),
        Column::new("ACTION", 18, Align::Left),
        Column::new("ID", 30, Align::Left),
    ]
}

/// One row; id = the monotonic `seq` (stable across cap
/// trims). TIME sorts as a string — zero-padded `HH:MM:SS`, so string
/// order IS chronological WITHIN a single day. Across midnight it is
/// not (an ascending TIME sort would put a late-night row first), so an
/// explicit ascending sort is a same-day view only. The default
/// newest-first order is `seq`-driven (`rows()`) and is unaffected.
pub fn to_row(row: &crate::core::EventRow) -> TableRow {
    TableRow::new(
        row.seq.to_string(),
        vec![
            row.event.timestamp.clone(),
            row.event.kind.clone(),
            row.event.action.clone(),
            row.event.actor.clone(),
        ],
    )
}

/// ALL buffered events, NEWEST FIRST.
pub fn rows(core: &AppCore) -> Vec<TableRow> {
    core.events.iter().rev().map(to_row).collect()
}

/// Info-line denominator: the buffered event count (counts).
pub fn total(core: &AppCore) -> usize {
    core.events.len()
}

const SPEC: ResourceSpec = ResourceSpec {
    title: "Events",
    number: 5,
    help_ctx: HELP_CTX,
    kind: None,
    columns,
    rows,
    total,
    empty: " Waiting for Docker events…",
    context_menu: None,
    picker: None,
};

/// Build the window (no CLI prefill — prefills Containers).
pub fn build(core: Rc<RefCell<AppCore>>, bounds: Rect, open: Rc<RefCell<bool>>) -> ResourceWindow {
    resource::build(SPEC, core, bounds, None, open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AppMsg, EventRow};
    use crate::domain::docker::events::DockerEvent;

    fn event(action: &str) -> DockerEvent {
        DockerEvent {
            timestamp: "12:00:00".into(),
            kind: "container".into(),
            action: action.into(),
            actor: "abc".into(),
        }
    }

    #[test]
    fn rows_are_newest_first_with_stable_seq_ids() {
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut()
            .apply(AppMsg::Events(vec![event("create"), event("start")]));
        let rows = rows(&core.borrow());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "2", "newest FIRST");
        assert_eq!(rows[1].id, "1");
        assert_eq!(
            rows[0].cells,
            vec!["12:00:00", "container", "start", "abc"],
            "TIME, TYPE, ACTION, ID"
        );
        // The same mapping the window drives — via `EventRow` direct.
        let direct = to_row(&EventRow {
            seq: 42,
            event: event("die"),
        });
        assert_eq!(direct.id, "42");
    }
}
