//! The Networks window : table + filter over
//! `AppCore::networks`.

use crate::core::{AppCore, PendingKind};
use crate::views::resource::{self, ResourceSpec, ResourceWindow};
use crate::widgets::resource_table::TableRow;
use crate::widgets::{Align, Column};
use std::cell::RefCell;
use std::rc::Rc;
use tv::widgets::SortKey;
use tv::{HelpCtx, Rect};

/// Help context while the Networks window is focused .
pub const HELP_CTX: HelpCtx = HelpCtx::custom("tvdocker.networks");

/// Column headers/formats are new (no oh-my-docker
/// equivalent row layout; no icons/colors, the theme has no per-kind
/// roles). Widths are ours, NOT copied: the 7-col total exceeds the
/// 76-cell interior and `layout_widths` shaves the widest (a
/// tested invariant).
pub fn columns() -> Vec<Column> {
    vec![
        Column::new("NAME", 22, Align::Left),
        Column::new("ID", 12, Align::Left),
        Column::new("DRIVER", 10, Align::Left),
        Column::new("SCOPE", 8, Align::Left),
        Column::new("SUBNET", 18, Align::Left),
        Column::new("GATEWAY", 18, Align::Left),
        Column::new("CONTAINERS", 11, Align::Right),
    ]
}

/// One row; id = the FULL network id, cell = 12 chars
/// (tvdocker's `id[..12]`, char-safe); CONTAINERS keyed.
pub fn to_row(n: &crate::domain::docker::networks::NetworkEntry) -> TableRow {
    TableRow::new(
        n.id.clone(),
        vec![
            n.name.clone(),
            n.id.chars().take(12).collect(),
            n.driver.clone(),
            n.scope.clone(),
            n.subnet.clone(),
            n.gateway.clone(),
            n.containers.to_string(),
        ],
    )
}

/// ALL network rows — unfiltered (the window owns fuzzy filtering).
pub fn rows(core: &AppCore) -> Vec<TableRow> {
    core.networks.iter().map(to_row).collect()
}

/// Info-line denominator: the live network count (counts).
pub fn total(core: &AppCore) -> usize {
    core.networks.len()
}

/// Raw sort keys index-aligned with [`rows`]: CONTAINERS (col 6) sorts on the
/// numeric count, not the text.
pub fn sort_keys(core: &AppCore) -> Vec<(usize, Vec<SortKey>)> {
    vec![(
        6,
        core.networks
            .iter()
            .map(|n| SortKey::Num(n.containers as f64))
            .collect(),
    )]
}

/// The Networks context menu (delete + confirm).
pub fn context_menu(_core: &AppCore, _id: &str) -> Vec<(String, tv::Command)> {
    vec![("~x~Delete".to_string(), crate::app::CMD_REQ_DELETE)]
}

const SPEC: ResourceSpec = ResourceSpec {
    title: "Networks",
    number: 3,
    help_ctx: HELP_CTX,
    kind: Some(PendingKind::Networks),
    columns,
    rows,
    total,
    empty: " No networks.",
    context_menu: Some(context_menu),
    table_key: "networks",
    sort_keys,
};

/// Build the window (no CLI prefill — prefills Containers).
pub fn build(core: Rc<RefCell<AppCore>>, bounds: Rect, open: Rc<RefCell<bool>>) -> ResourceWindow {
    resource::build(SPEC, core, bounds, None, open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::docker::networks::NetworkEntry;

    #[test]
    fn to_row_maps_every_cell_and_keys_the_container_count() {
        let n = NetworkEntry {
            id: "abcdef0123456789".into(),
            name: "bridge".into(),
            driver: "bridge".into(),
            scope: "local".into(),
            subnet: "172.18.0.0/16".into(),
            gateway: "172.18.0.1".into(),
            containers: 3,
        };
        let r = to_row(&n);
        assert_eq!(r.id, "abcdef0123456789", "full id");
        assert_eq!(
            r.cells,
            vec![
                "bridge",
                "abcdef012345",
                "bridge",
                "local",
                "172.18.0.0/16",
                "172.18.0.1",
                "3"
            ],
            "12-char ID cell, tvdocker columns"
        );
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut()
            .apply(crate::core::AppMsg::Networks(vec![n]));
        assert_eq!(
            sort_keys(&core.borrow()),
            vec![(6, vec![SortKey::Num(3.0)])],
            "CONTAINERS sorts numerically"
        );
    }

    #[test]
    fn context_menu_binds_delete_with_a_scoped_hot_letter() {
        let core = crate::core::AppCore::new();
        let items = context_menu(&core, "net1");
        assert_eq!(
            items,
            vec![("~x~Delete".to_string(), crate::app::CMD_REQ_DELETE)],
            "one item; hot letter scoped to the open menu "
        );
    }
}
