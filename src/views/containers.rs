//! The Containers window : the spec + column/row mapping onto
//! the generic `views::resource::ResourceWindow`.

use crate::core::{AppCore, PendingKind};
use crate::views::resource::{self, ResourceSpec, ResourceWindow};
use crate::widgets::resource_table::TableRow;
use crate::widgets::{Align, Column};
use std::cell::RefCell;
use std::rc::Rc;
use tv::widgets::SortKey;
use tv::{HelpCtx, Rect};

/// Help context while the Containers window is focused
/// (the status `def_one_of` lists all six).
pub const HELP_CTX: HelpCtx = HelpCtx::custom("tvdocker.containers");

/// Columns of the Containers table
pub fn columns() -> Vec<Column> {
    vec![
        Column::new("NAME", 26, Align::Left),
        Column::new("IMAGE", 28, Align::Left),
        Column::new("STATE", 10, Align::Left),
        Column::new("STATUS", 24, Align::Left),
        Column::new("PORTS", 18, Align::Left),
    ]
}

/// One table row; the stable id is the container id .
pub fn to_row(c: &crate::domain::docker::containers::ContainerSummary) -> TableRow {
    TableRow::new(
        c.id.clone(),
        vec![
            c.name.clone(),
            c.image.clone(),
            c.state.clone(),
            c.status.clone(),
            c.ports.clone(),
        ],
    )
}

/// ALL container rows — unfiltered (the window fuzzies over cells,
/// which hold exactly the fields the old matcher read).
pub fn rows(core: &AppCore) -> Vec<TableRow> {
    core.containers.iter().map(to_row).collect()
}

/// Info-line denominator: the live container count (counts).
pub fn total(core: &AppCore) -> usize {
    core.containers.len()
}

/// Raw per-column sort keys, index-aligned with [`rows`] — every container
/// cell sorts as text (no hidden numeric value), so this is empty.
pub fn sort_keys(_core: &AppCore) -> Vec<(usize, Vec<SortKey>)> {
    Vec::new()
}

/// The container context menu: hot
/// letters are scoped to the open menu ; Start/Stop
/// flips on the ROW's state (the desktop filters the target set
/// by state).
pub fn context_menu(core: &AppCore, id: &str) -> Vec<(String, tv::Command)> {
    let running = core
        .containers
        .iter()
        .find(|c| c.id == id)
        .is_some_and(|c| c.state == "running");
    vec![
        ("~E~xplorer".to_string(), crate::app::CMD_REQ_EXPLORER),
        ("~I~mage".to_string(), crate::app::CMD_REQ_IMAGE),
        ("~D~etails".to_string(), crate::app::CMD_REQ_DETAILS),
        ("~L~ogs".to_string(), crate::app::CMD_REQ_LOGS),
        ("~S~hell".to_string(), crate::app::CMD_REQ_SHELL),
        ("Diag~n~ose…".to_string(), crate::app::CMD_REQ_DIAGNOSTICS),
        (
            if running { "s~T~op" } else { "s~t~art" }.to_string(),
            if running {
                crate::app::CMD_REQ_STOP
            } else {
                crate::app::CMD_REQ_START
            },
        ),
        ("~x~Delete".to_string(), crate::app::CMD_REQ_DELETE),
    ]
}

/// The Containers spec.
const SPEC: ResourceSpec = ResourceSpec {
    title: "Containers",
    number: 1,
    help_ctx: HELP_CTX,
    kind: Some(PendingKind::Containers),
    columns,
    rows,
    total,
    empty: " No containers.",
    context_menu: Some(context_menu),
    table_key: "containers",
    sort_keys,
};

/// Build the window — **signature unchanged** so `chrome.rs`,
/// `esc_chain.rs` and `docker_menu.rs` keep compiling.
/// `filter` prefills the filter line (CLI prefill).
pub fn build(
    core: Rc<RefCell<AppCore>>,
    bounds: Rect,
    filter: Option<&str>,
    open: Rc<RefCell<bool>>,
) -> ResourceWindow {
    resource::build(SPEC, core, bounds, filter, open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AppMsg, Connection};
    use crate::testutil::{render_view, send_event, text};
    use tv::event::Event;

    fn container(
        id: &str,
        name: &str,
        state: &str,
    ) -> crate::domain::docker::containers::ContainerSummary {
        crate::domain::docker::containers::ContainerSummary {
            id: id.into(),
            name: name.into(),
            image: "img".into(),
            image_id: "sha256:img".into(),
            state: state.into(),
            status: "Up".into(),
            ports: "80/tcp".into(),
            project: String::new(),
            health: String::new(),
        }
    }

    fn core_with(
        rows: Vec<crate::domain::docker::containers::ContainerSummary>,
    ) -> Rc<RefCell<AppCore>> {
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut().apply(AppMsg::Containers(rows));
        core
    }

    fn window(
        core: Rc<RefCell<AppCore>>,
        filter: Option<&str>,
    ) -> (ResourceWindow, Rc<RefCell<bool>>) {
        let open = Rc::new(RefCell::new(true));
        let w = build(core, Rect::new(2, 1, 78, 23), filter, Rc::clone(&open));
        (w, open)
    }

    #[test]
    fn rows_map_from_core_on_refresh() {
        let core = core_with(vec![
            container("id1", "web", "running"),
            container("id2", "db", "exited"),
        ]);
        let (mut w, _open) = window(core.clone(), None);
        assert_eq!(w.table_ids(), vec!["id1", "id2"], "initial rows");
        // A poll lands: core changes, bridge broadcasts REFRESH.
        core.borrow_mut().apply(AppMsg::Containers(vec![container(
            "id3", "cache", "running",
        )]));
        send_event(
            &mut w,
            Event::Broadcast {
                command: crate::bridge::REFRESH,
                source: None,
            },
        );
        assert_eq!(
            w.table_ids(),
            vec!["id3"],
            "rows re-mapped on revision change"
        );
    }

    #[test]
    fn info_line_shows_unreachable_empty_and_counts() {
        let core = core_with(vec![
            container("id1", "web", "running"),
            container("id2", "db", "exited"),
        ]);
        // The poller sends Connection(Up) with a successful list (message
        // order in spawn_containers) — mirror it.
        core.borrow_mut().apply(AppMsg::Connection(Connection::Up));
        let (mut w, _open) = window(core.clone(), None);
        let out = text(&render_view(76, 23, &mut w));
        assert!(out.contains("2 / 2 shown"), "shown/total counts: {out:?}");

        core.borrow_mut()
            .apply(AppMsg::Connection(Connection::Down { error: "x".into() }));
        let out = text(&render_view(76, 23, &mut w));
        assert!(
            out.contains("Docker unreachable — retrying…"),
            "empty state: {out:?}"
        );

        core.borrow_mut().apply(AppMsg::Connection(Connection::Up));
        core.borrow_mut().apply(AppMsg::Containers(vec![]));
        let out = text(&render_view(76, 23, &mut w));
        assert!(out.contains("No containers."), "empty list: {out:?}");
    }

    /// The Diagnose item exists and carries its command (Review Focus #1).
    ///
    /// Ruling: the raw-label assertion
    /// `label.contains("iagnose")` misses because the hot-letter
    /// markers split it (`"Diag~n~ose…"`), so it can never match. Strip the
    /// markers first — the assertion keeps the intended meaning (the
    /// visible text is "Diagnose…").
    #[test]
    fn context_menu_offers_diagnose() {
        let core = core_with(vec![container("id-a", "web", "running")]);
        let items = context_menu(&core.borrow(), "id-a");
        assert!(
            items
                .iter()
                .any(|(label, cmd)| label.replace('~', "").contains("iagnose")
                    && *cmd == crate::app::CMD_REQ_DIAGNOSTICS),
            "Diagnose… present: {items:?}"
        );
        // The hot letter must be unique within the menu.
        let n = items
            .iter()
            .filter(|(l, _)| l.replace('~', "").to_lowercase().contains('n'))
            .count();
        assert_eq!(n, 1, "exactly one 'n' hotspot: {items:?}");
    }

    /// The "Image" jump exists and is the only `i`/`I` hotspot
    /// (the hot letter is the `~`-marked char — a raw-text scan would
    /// false-match "Di**i**agnose…"'s unmarked letters).
    #[test]
    fn context_menu_offers_image() {
        let core = core_with(vec![container("id-a", "web", "running")]);
        let items = context_menu(&core.borrow(), "id-a");
        assert!(
            items
                .iter()
                .any(|(l, c)| { l.replace('~', "") == "Image" && *c == crate::app::CMD_REQ_IMAGE }),
            "Image present: {items:?}"
        );
        let hot = |l: &str| -> Vec<char> {
            let b = l.as_bytes();
            (1..b.len().saturating_sub(1))
                .filter(|&i| b[i - 1] == b'~' && b[i + 1] == b'~')
                .filter_map(|i| l[i..].chars().next())
                .collect()
        };
        let n = items
            .iter()
            .filter(|(l, _)| hot(l).iter().any(|c| *c == 'i' || *c == 'I'))
            .count();
        assert_eq!(n, 1, "exactly one 'i' hotspot: {items:?}");
    }
}
