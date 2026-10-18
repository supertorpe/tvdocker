//! The Images window : table + filter over `AppCore::images`.

use crate::core::{AppCore, PendingKind};
use crate::views::resource::{self, ResourceSpec, ResourceWindow, format_size};
use crate::widgets::resource_table::TableRow;
use crate::widgets::{Align, Column};
use std::cell::RefCell;
use std::rc::Rc;
use tv::widgets::SortKey;
use tv::{HelpCtx, Rect};

/// Help context while the Images window is focused .
pub const HELP_CTX: HelpCtx = HelpCtx::custom("tvdocker.images");

/// Columns (tvdocker parity): REPOSITORY, TAG, SIZE, ID.
pub fn columns() -> Vec<Column> {
    vec![
        Column::new("REPOSITORY", 24, Align::Left),
        Column::new("TAG", 16, Align::Left),
        Column::new("SIZE", 10, Align::Right),
        Column::new("ID", 14, Align::Left),
    ]
}

/// One row; id = the FULL image id. SIZE shows the
/// tvdocker-formatted bytes but sorts on the RAW bytes;
/// ID shows the first 12 chars (char-safe form of the vendored slice).
pub fn to_row(i: &crate::domain::docker::images::ImageEntry) -> TableRow {
    TableRow::new(
        i.id.clone(),
        vec![
            i.repository.clone(),
            i.tag.clone(),
            format_size(i.size),
            i.id.chars().take(12).collect(),
        ],
    )
}

/// ALL image rows — unfiltered (the window owns fuzzy filtering).
pub fn rows(core: &AppCore) -> Vec<TableRow> {
    core.images.iter().map(to_row).collect()
}

/// Info-line denominator: the live image count (counts).
pub fn total(core: &AppCore) -> usize {
    core.images.len()
}

/// Raw sort keys index-aligned with [`rows`]: SIZE (col 2) sorts on the raw
/// bytes, not the `1.5GB` display text.
pub fn sort_keys(core: &AppCore) -> Vec<(usize, Vec<SortKey>)> {
    vec![(
        2,
        core.images
            .iter()
            .map(|i| SortKey::Num(i.size as f64))
            .collect(),
    )]
}

/// The Images-window row id for a container's image: the container's
/// `image_id` wins (exact — the only match that survives multi-tag
/// images, whose rows keep just the first repo tag); then the
/// `repo:tag` / implicit-`latest` name forms. `None` when the image is
/// not in the list (pruned while the container outlived it).
pub fn image_row_id_for_container(core: &AppCore, container_id: &str) -> Option<String> {
    let container = core.containers.iter().find(|c| c.id == container_id)?;
    if !container.image_id.is_empty()
        && let Some(row) = core.images.iter().find(|i| i.id == container.image_id)
    {
        return Some(row.id.clone());
    }
    let name =
        |i: &crate::domain::docker::images::ImageEntry| format!("{}:{}", i.repository, i.tag);
    core.images
        .iter()
        .find(|i| {
            name(i) == container.image || (container.image == i.repository && i.tag == "latest")
        })
        .map(|i| i.id.clone())
}

/// The Images context menu: run form, remove,
/// and the two batch sweeps. Hot letters are menu-scoped .
pub fn context_menu(_core: &AppCore, _id: &str) -> Vec<(String, tv::Command)> {
    vec![
        ("~E~xplorer".to_string(), crate::app::CMD_REQ_IMAGE_EXPLORER),
        ("~L~ayers…".to_string(), crate::app::CMD_REQ_IMAGE_LAYERS),
        ("~R~un…".to_string(), crate::app::CMD_REQ_RUN),
        ("~x~Remove".to_string(), crate::app::CMD_REQ_DELETE),
        (
            "remove ~d~angling".to_string(),
            crate::app::CMD_REQ_DANGLING,
        ),
        ("~p~rune unused".to_string(), crate::app::CMD_REQ_PRUNE),
    ]
}

const SPEC: ResourceSpec = ResourceSpec {
    title: "Images",
    number: 2,
    help_ctx: HELP_CTX,
    kind: Some(PendingKind::Images),
    columns,
    rows,
    total,
    empty: " No images.",
    context_menu: Some(context_menu),
    table_key: "images",
    sort_keys,
};

/// Build the window (no CLI prefill — prefills Containers).
pub fn build(core: Rc<RefCell<AppCore>>, bounds: Rect, open: Rc<RefCell<bool>>) -> ResourceWindow {
    resource::build(SPEC, core, bounds, None, open)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> crate::domain::docker::images::ImageEntry {
        crate::domain::docker::images::ImageEntry {
            id: "sha256:1111222233334444".into(),
            repository: "nginx".into(),
            tag: "1.27".into(),
            size: 1_500_000_000,
        }
    }

    #[test]
    fn to_row_maps_cells_truncates_id_and_keys_the_size() {
        let r = to_row(&entry());
        assert_eq!(r.id, "sha256:1111222233334444", "id = full");
        assert_eq!(
            r.cells,
            vec!["nginx", "1.27", "1.5GB", "sha256:11112"],
            "SIZE formats tvdocker-style; ID shows 12 chars"
        );
        let core = Rc::new(RefCell::new(AppCore::new()));
        core.borrow_mut()
            .apply(crate::core::AppMsg::Images(vec![entry()]));
        assert_eq!(
            sort_keys(&core.borrow()),
            vec![(2, vec![SortKey::Num(1_500_000_000.0)])],
            "SIZE sorts on raw bytes"
        );
    }

    #[test]
    fn context_menu_binds_layers_run_remove_dangling_and_prune() {
        let core = crate::core::AppCore::new();
        let items = context_menu(&core, "sha256:aaa");
        let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "~E~xplorer",
                "~L~ayers…",
                "~R~un…",
                "~x~Remove",
                "remove ~d~angling",
                "~p~rune unused"
            ],
            "hot letters are unique within the menu "
        );
        assert_eq!(items[0].1, crate::app::CMD_REQ_IMAGE_EXPLORER);
        assert_eq!(items[1].1, crate::app::CMD_REQ_IMAGE_LAYERS);
        assert_eq!(items[2].1, crate::app::CMD_REQ_RUN);
        assert_eq!(items[3].1, crate::app::CMD_REQ_DELETE);
        assert_eq!(items[4].1, crate::app::CMD_REQ_DANGLING);
        assert_eq!(items[5].1, crate::app::CMD_REQ_PRUNE);
    }

    /// The Explorer entry exists and is the only `e`/`E` hotspot.
    #[test]
    fn the_images_menu_offers_explorer() {
        let items = context_menu(&AppCore::new(), "sha256:abc");
        assert!(
            items.iter().any(|(l, c)| {
                l.replace('~', "") == "Explorer" && *c == crate::app::CMD_REQ_IMAGE_EXPLORER
            }),
            "Explorer… present: {items:?}"
        );
        let hot = items
            .iter()
            .filter(|(l, _)| l.to_lowercase().contains("~e~"))
            .count();
        assert_eq!(hot, 1, "exactly one `e` hotspot: {items:?}");
    }

    fn img(id: &str, repo: &str, tag: &str) -> crate::domain::docker::images::ImageEntry {
        crate::domain::docker::images::ImageEntry {
            id: id.into(),
            repository: repo.into(),
            tag: tag.into(),
            size: 1_000,
        }
    }

    fn cont(
        id: &str,
        image: &str,
        image_id: &str,
    ) -> crate::domain::docker::containers::ContainerSummary {
        crate::domain::docker::containers::ContainerSummary {
            id: id.into(),
            name: id.into(),
            image: image.into(),
            image_id: image_id.into(),
            state: "running".into(),
            status: "Up".into(),
            ports: String::new(),
            project: String::new(),
            health: String::new(),
        }
    }

    /// The jump resolves a container to its Images-window row id.
    #[test]
    fn image_row_id_resolves_by_id_then_repo_tag_then_implicit_latest() {
        let mut core = AppCore::new();
        core.apply(crate::core::AppMsg::Images(vec![
            img("sha256:aaa", "nginx", "1.27"),
            img("sha256:bbb", "redis", "latest"),
            // Multi-tag image: `summarize` keeps the FIRST repo tag, so a
            // container using the other tag can only resolve by id.
            img("sha256:ccc", "app", "prod"),
        ]));
        core.apply(crate::core::AppMsg::Containers(vec![
            // Exact id match — the primary path.
            cont("c1", "something-else:1", "sha256:aaa"),
            // `repo:tag` name match.
            cont("c2", "redis:latest", "sha256:missing"),
            // Implicit `latest` (`docker run nginx` → row `nginx:latest`).
            cont("c3", "redis", "sha256:missing"),
            // No image row at all.
            cont("c4", "gone:1", "sha256:missing"),
        ]));
        assert_eq!(
            image_row_id_for_container(&core, "c1").as_deref(),
            Some("sha256:aaa"),
            "container image id wins"
        );
        assert_eq!(
            image_row_id_for_container(&core, "c2").as_deref(),
            Some("sha256:bbb"),
            "repo:tag name match"
        );
        assert_eq!(
            image_row_id_for_container(&core, "c3").as_deref(),
            Some("sha256:bbb"),
            "implicit latest"
        );
        assert_eq!(
            image_row_id_for_container(&core, "c4"),
            None,
            "pruned image: no row to jump to"
        );
        assert_eq!(
            image_row_id_for_container(&core, "unknown"),
            None,
            "unknown container id"
        );
    }
}
