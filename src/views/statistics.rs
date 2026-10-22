//! The Statistics window : the live stats table over
//! `AppCore::statistics` — CPU, mem, net I/O, block I/O, PIDs; header
//! click or ←/→ sorts (every numeric column carries a sort key).

use crate::core::AppCore;
use crate::views::resource::{self, ResourceSpec, ResourceWindow};
use crate::widgets::resource_table::TableRow;
use crate::widgets::{Align, Column};
use std::cell::RefCell;
use std::rc::Rc;
use tv::widgets::SortKey;
use tv::{HelpCtx, Rect};

/// Help context while the Statistics window is focused .
pub const HELP_CTX: HelpCtx = HelpCtx::custom("tvdocker.statistics");

/// numeric columns right-align.
pub fn columns() -> Vec<Column> {
    vec![
        Column::new("NAME", 24, Align::Left),
        Column::new("CPU %", 8, Align::Right),
        Column::new("MEM USAGE/LIMIT", 22, Align::Right),
        Column::new("NET I/O", 16, Align::Right),
        Column::new("BLOCK I/O", 16, Align::Right),
        Column::new("PIDS", 6, Align::Right),
    ]
}

/// One row; id = the CONTAINER id (the stats poller keys its rows by
/// container, so the container-scoped actions can target it); cell formats
/// copied verbatim from oh-my-docker's statistics UI; every numeric column
/// sorts on its raw value.
pub fn to_row(s: &crate::domain::docker::statistics::StatEntry) -> TableRow {
    let mem = format!(
        "{:.1}MiB / {:.1}MiB",
        s.memory_usage as f64 / 1_048_576.0,
        s.memory_limit as f64 / 1_048_576.0
    );
    let net = format!(
        "{:.1}MB / {:.1}MB",
        s.net_rx as f64 / 1_000_000.0,
        s.net_tx as f64 / 1_000_000.0
    );
    let block = if s.block_read > 0 || s.block_write > 0 {
        format!(
            "{:.1}MB / {:.1}MB",
            s.block_read as f64 / 1_000_000.0,
            s.block_write as f64 / 1_000_000.0
        )
    } else {
        "0B / 0B".to_string()
    };
    TableRow::new(
        // An id-less sample (never seen from the poller) falls back to
        // the name, so the row keeps a usable key.
        if s.id.is_empty() {
            s.name.clone()
        } else {
            s.id.clone()
        },
        vec![
            s.name.clone(),
            format!("{:.1}%", s.cpu_percent),
            mem,
            net,
            block,
            s.pids.to_string(),
        ],
    )
}

/// ALL stats rows — unfiltered (the window owns fuzzy filtering).
pub fn rows(core: &AppCore) -> Vec<TableRow> {
    core.statistics.iter().map(to_row).collect()
}

/// Info-line denominator: the live stats row count (counts).
pub fn total(core: &AppCore) -> usize {
    core.statistics.len()
}

/// Raw sort keys index-aligned with [`rows`] — every numeric column
/// (CPU %, MEM, NET I/O, BLOCK I/O, PIDS) sorts on its underlying value, not
/// the formatted text.
pub fn sort_keys(core: &AppCore) -> Vec<(usize, Vec<SortKey>)> {
    let stats = &core.statistics;
    vec![
        (
            1,
            stats.iter().map(|s| SortKey::Num(s.cpu_percent)).collect(),
        ),
        (
            2,
            stats
                .iter()
                .map(|s| SortKey::Num(s.memory_usage as f64))
                .collect(),
        ),
        (
            3,
            stats
                .iter()
                .map(|s| SortKey::Num((s.net_rx + s.net_tx) as f64))
                .collect(),
        ),
        (
            4,
            stats
                .iter()
                .map(|s| SortKey::Num((s.block_read + s.block_write) as f64))
                .collect(),
        ),
        (
            5,
            stats.iter().map(|s| SortKey::Num(s.pids as f64)).collect(),
        ),
    ]
}

/// The Statistics row menu — the Containers menu, applied to the row's
/// container (the row's id IS the container id). A stats row is a RUNNING
/// container by construction (`docker stats` only samples running ones),
/// so the Start/Stop flip does not depend on the containers list, which
/// polls separately.
pub fn context_menu(_core: &AppCore, _id: &str) -> Vec<(String, tv::Command)> {
    crate::views::containers::context_menu_for(true)
}

const SPEC: ResourceSpec = ResourceSpec {
    title: "Statistics",
    number: 6,
    help_ctx: HELP_CTX,
    kind: None,
    columns,
    rows,
    total,
    empty: " No running containers.",
    context_menu: Some(context_menu),
    table_key: "statistics",
    sort_keys,
};

/// Build the window (no CLI prefill — prefills Containers).
pub fn build(core: Rc<RefCell<AppCore>>, bounds: Rect, open: Rc<RefCell<bool>>) -> ResourceWindow {
    resource::build(SPEC, core, bounds, None, open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::docker::statistics::StatEntry;

    fn entry() -> StatEntry {
        StatEntry {
            id: "id-web".into(),
            name: "web".into(),
            cpu_percent: 9.9,
            memory_usage: 104_857_600,
            memory_limit: 524_288_000,
            memory_percent: 20.0,
            net_rx: 1_000_000,
            net_tx: 2_000_000,
            block_read: 0,
            block_write: 0,
            pids: 4,
        }
    }

    /// The row's id must be the CONTAINER id: every menu item acts on a
    /// container id (the desktop's openers queue ids, not names).
    #[test]
    fn the_stats_row_is_keyed_by_container_id() {
        let r = to_row(&entry());
        assert_eq!(r.id, "id-web", "id = the container id");
        assert_eq!(r.cells[0], "web", "the NAME cell is unchanged");
    }

    /// The Statistics window carries the Containers row menu: the same
    /// items, on the row's container.
    #[test]
    fn the_stats_menu_is_the_containers_menu() {
        let core = Rc::new(RefCell::new(AppCore::new()));
        let items = context_menu(&core.borrow(), "id-web");
        assert_eq!(
            items,
            crate::views::containers::context_menu_for(true),
            "the Containers menu, verbatim"
        );
        assert!(
            items.iter().any(|(l, _)| l.replace('~', "") == "Image"),
            "the Image jump is there too: {items:?}"
        );
        assert!(
            items
                .iter()
                .any(|(l, _)| l.replace('~', "").to_lowercase() == "stop"),
            "a stats row is a RUNNING container ⇒ Stop: {items:?}"
        );
    }

    #[test]
    fn statistics_rows_carry_numeric_sort_keys_and_tvdocker_formats() {
        // Review Focus #5: the keys make CPU 9.9% sort BELOW 10.2%.
        let r = to_row(&entry());
        assert_eq!(r.id, "id-web", "id = the container id");
        assert_eq!(r.cells[1], "9.9%", "CPU % format");
        assert_eq!(r.cells[2], "100.0MiB / 500.0MiB", "MEM in MiB");
        assert_eq!(r.cells[3], "1.0MB / 2.0MB", "NET rx / tx in MB");
        assert_eq!(r.cells[4], "0B / 0B", "zero block I/O shorthand");
        assert_eq!(r.cells[5], "4");
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut()
            .apply(crate::core::AppMsg::Statistics(vec![entry()]));
        assert_eq!(
            sort_keys(&core.borrow()),
            vec![
                (1, vec![SortKey::Num(9.9)]),
                (2, vec![SortKey::Num(104_857_600.0)]),
                (3, vec![SortKey::Num(3_000_000.0)]),
                (4, vec![SortKey::Num(0.0)]),
                (5, vec![SortKey::Num(4.0)]),
            ],
            "every numeric column sorts on its raw value"
        );
    }

    #[test]
    fn nonzero_block_io_drops_the_shorthand() {
        let mut s = entry();
        s.block_read = 3_000_000;
        s.block_write = 1_500_000;
        let r = to_row(&s);
        assert_eq!(r.cells[4], "3.0MB / 1.5MB");
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut()
            .apply(crate::core::AppMsg::Statistics(vec![s]));
        assert_eq!(
            sort_keys(&core.borrow())[3].1,
            vec![SortKey::Num(4_500_000.0)],
            "block I/O sorts on the read+write sum"
        );
    }
}
