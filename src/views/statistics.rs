//! The Statistics window : the live stats table over
//! `AppCore::statistics` — CPU, mem, net I/O, block I/O, PIDs; header
//! click or ←/→ sorts (every numeric column carries a sort key).

use crate::core::AppCore;
use crate::views::resource::{self, ResourceSpec, ResourceWindow};
use crate::widgets::{Align, Column, TableRow};
use std::cell::RefCell;
use std::rc::Rc;
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

/// One row; id = the container NAME (— the stats poller
/// keys by name). Cell formats copied verbatim from oh-my-docker's
/// statistics UI; every numeric column sorts on its raw
/// value.
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
        s.name.clone(),
        vec![
            s.name.clone(),
            format!("{:.1}%", s.cpu_percent),
            mem,
            net,
            block,
            s.pids.to_string(),
        ],
    )
    .with_sort_keys(vec![
        None,
        Some(s.cpu_percent),
        Some(s.memory_usage as f64),
        Some((s.net_rx + s.net_tx) as f64),
        Some((s.block_read + s.block_write) as f64),
        Some(s.pids as f64),
    ])
}

/// ALL stats rows — unfiltered (the window owns fuzzy filtering).
pub fn rows(core: &AppCore) -> Vec<TableRow> {
    core.statistics.iter().map(to_row).collect()
}

/// Info-line denominator: the live stats row count (counts).
pub fn total(core: &AppCore) -> usize {
    core.statistics.len()
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
    use crate::domain::docker::statistics::StatEntry;

    fn entry() -> StatEntry {
        StatEntry {
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

    #[test]
    fn statistics_rows_carry_numeric_sort_keys_and_tvdocker_formats() {
        // Review Focus #5: the keys make CPU 9.9% sort BELOW 10.2%.
        let r = to_row(&entry());
        assert_eq!(r.id, "web", "id = container name");
        assert_eq!(r.cells[1], "9.9%", "CPU % format");
        assert_eq!(r.cells[2], "100.0MiB / 500.0MiB", "MEM in MiB");
        assert_eq!(r.cells[3], "1.0MB / 2.0MB", "NET rx / tx in MB");
        assert_eq!(r.cells[4], "0B / 0B", "zero block I/O shorthand");
        assert_eq!(r.cells[5], "4");
        assert_eq!(
            r.sort_keys,
            vec![
                None,
                Some(9.9),
                Some(104_857_600.0),
                Some(3_000_000.0),
                Some(0.0),
                Some(4.0),
            ],
            "every numeric column keyed"
        );
    }

    #[test]
    fn nonzero_block_io_drops_the_shorthand() {
        let mut s = entry();
        s.block_read = 3_000_000;
        s.block_write = 1_500_000;
        let r = to_row(&s);
        assert_eq!(r.cells[4], "3.0MB / 1.5MB");
        assert_eq!(r.sort_keys[4], Some(4_500_000.0), "sum key");
    }
}
