//! The Volumes window : table + filter over `AppCore::volumes`.

use crate::core::{AppCore, PendingKind};
use crate::views::resource::{self, ResourceSpec, ResourceWindow, format_size};
use crate::widgets::resource_table::TableRow;
use crate::widgets::{Align, Column};
use std::cell::RefCell;
use std::rc::Rc;
use tv::widgets::SortKey;
use tv::{HelpCtx, Rect};

/// Help context while the Volumes window is focused .
pub const HELP_CTX: HelpCtx = HelpCtx::custom("tvdocker.volumes");

/// Columns (tvdocker parity): NAME, DRIVER, SIZE,
/// MOUNTPOINT.
pub fn columns() -> Vec<Column> {
    vec![
        Column::new("NAME", 22, Align::Left),
        Column::new("DRIVER", 10, Align::Left),
        Column::new("SIZE", 9, Align::Right),
        Column::new("MOUNTPOINT", 40, Align::Left),
    ]
}

/// One row; id = the volume NAME (— names are the Docker
/// identity); SIZE formats for display, sorts on raw bytes.
pub fn to_row(v: &crate::domain::docker::volumes::VolumeEntry) -> TableRow {
    TableRow::new(
        v.name.clone(),
        vec![
            v.name.clone(),
            v.driver.clone(),
            format_size(v.size),
            v.mountpoint.clone(),
        ],
    )
}

/// ALL volume rows — unfiltered (the window owns fuzzy filtering).
pub fn rows(core: &AppCore) -> Vec<TableRow> {
    core.volumes.iter().map(to_row).collect()
}

/// Info-line denominator: the live volume count (counts).
pub fn total(core: &AppCore) -> usize {
    core.volumes.len()
}

/// Raw sort keys index-aligned with [`rows`]: SIZE (col 2) sorts on the raw
/// bytes, not the `1.5MB` display text.
pub fn sort_keys(core: &AppCore) -> Vec<(usize, Vec<SortKey>)> {
    vec![(
        2,
        core.volumes
            .iter()
            .map(|v| SortKey::Num(v.size as f64))
            .collect(),
    )]
}

/// The Volumes context menu (delete + confirm; Enter opens Explorer).
pub fn context_menu(_core: &AppCore, _id: &str) -> Vec<(String, tv::Command)> {
    vec![
        (
            "~E~xplorer".to_string(),
            crate::app::CMD_REQ_VOLUME_EXPLORER,
        ),
        ("~x~Delete".to_string(), crate::app::CMD_REQ_DELETE),
    ]
}

const SPEC: ResourceSpec = ResourceSpec {
    title: "Volumes",
    number: 4,
    help_ctx: HELP_CTX,
    kind: Some(PendingKind::Volumes),
    columns,
    rows,
    total,
    empty: " No volumes.",
    context_menu: Some(context_menu),
    table_key: "volumes",
    sort_keys,
};

/// Build the window (no CLI prefill — prefills Containers).
pub fn build(core: Rc<RefCell<AppCore>>, bounds: Rect, open: Rc<RefCell<bool>>) -> ResourceWindow {
    resource::build(SPEC, core, bounds, None, open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::docker::volumes::VolumeEntry;

    #[test]
    fn to_row_formats_size_and_carries_the_bytes_key() {
        let v = VolumeEntry {
            name: "data".into(),
            driver: "local".into(),
            mountpoint: "/var/lib/docker/volumes/data/_data".into(),
            size: 1_500_000,
        };
        let r = to_row(&v);
        assert_eq!(r.id, "data", "id = name");
        assert_eq!(
            r.cells,
            vec![
                "data",
                "local",
                "1.5MB",
                "/var/lib/docker/volumes/data/_data"
            ]
        );
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut()
            .apply(crate::core::AppMsg::Volumes(vec![v]));
        assert_eq!(
            sort_keys(&core.borrow()),
            vec![(2, vec![SortKey::Num(1_500_000.0)])],
            "SIZE sorts on raw bytes"
        );
    }

    #[test]
    fn context_menu_binds_delete_with_a_scoped_hot_letter() {
        let core = crate::core::AppCore::new();
        let items = context_menu(&core, "data");
        assert_eq!(
            items,
            vec![
                (
                    "~E~xplorer".to_string(),
                    crate::app::CMD_REQ_VOLUME_EXPLORER
                ),
                ("~x~Delete".to_string(), crate::app::CMD_REQ_DELETE),
            ],
            "two items; hot letter scoped to the open menu "
        );
    }
}
