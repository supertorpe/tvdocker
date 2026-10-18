//! The fidelity seam between tvdocker's resource rows and the upstream
//! tvision-rs `Table`: a [`TableModel`] over [`TableRow`]s.
//!
//! This module is pure — it holds no views — so the row shapes the resource
//! builders produce can be unit-tested away from the widget tree.
use std::borrow::Cow;

use tv::Role;
use tv::widgets::table::{ColumnSpec, RowKey, SortKey, TableModel};

/// Column alignment inside its cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// One column definition: label, preferred width, alignment. The row builders
/// speak `Column`; [`to_spec`] turns it into the upstream [`ColumnSpec`].
#[derive(Clone, Debug)]
pub struct Column {
    pub label: String,
    pub width: i32,
    pub align: Align,
    /// Stretch weight (0 = fixed): spare cells go to the flex columns by
    /// weight, so a column can claim "whatever is left" without knowing the
    /// pane width up front (upstream `ColumnSpec::flex`).
    pub flex: u16,
    /// The narrowest the layout may shrink this column to (upstream
    /// `ColumnSpec::min_width`, at least 1).
    pub min_width: i32,
}

impl Column {
    /// A column with `label`, preferred `width` cells, aligned per `align`.
    pub fn new(label: impl Into<String>, width: i32, align: Align) -> Self {
        Self {
            label: label.into(),
            width,
            align,
            flex: 0,
            min_width: 1,
        }
    }

    /// Builder: let this column stretch into spare width with `weight`
    /// (0 = fixed). Combined with [`min_width`](Self::min_width) the column
    /// fills the pane but never drops below the floor — a narrower pane
    /// overflows and scrolls instead of squeezing it.
    pub fn flex(mut self, weight: u16) -> Self {
        self.flex = weight;
        self
    }

    /// Builder: the narrowest the layout may shrink this column to.
    pub fn min_width(mut self, min_width: i32) -> Self {
        self.min_width = min_width;
        self
    }
}

/// Translate a tvdocker [`Column`] into an upstream [`ColumnSpec`]: the label
/// and preferred width always, right-aligned when the column asks for it,
/// carrying the stretch weight and the width floor when it has them.
pub fn to_spec(col: &Column) -> ColumnSpec {
    let mut spec = ColumnSpec::new(col.label.clone(), col.width);
    if col.flex > 0 {
        spec = spec.flex(col.flex);
    }
    if col.min_width > 1 {
        spec = spec.min_width(col.min_width);
    }
    match col.align {
        Align::Left => spec,
        Align::Right => spec.right(),
    }
}

/// One display row: a stable id (cursor/selection identity) plus cell text,
/// with an optional per-row tint.
#[derive(Clone, Debug)]
pub struct TableRow {
    pub id: String,
    pub cells: Vec<String>,
    /// Replaces only the normal row colour (focus/selected still win).
    pub role: Option<Role>,
}

impl TableRow {
    /// A plain row (no tint).
    pub fn new(id: impl Into<String>, cells: Vec<String>) -> Self {
        Self {
            id: id.into(),
            cells,
            role: None,
        }
    }

    /// Builder: tint the row with `role` (see [`TableModel::row_role`]).
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Builder: tint the row only when `role` is `Some` (the change tree
    /// maps `DiffType` to roles, `None` meaning "no tint").
    pub fn with_opt_role(mut self, role: Option<Role>) -> Self {
        self.role = role;
        self
    }
}

/// `TableRow`s as an upstream [`TableModel`], with an optional per-(col,row)
/// numeric/text sort-key override (the old `with_sort_keys`).
pub struct RowModel {
    rows: Vec<TableRow>,
    /// `keys[col][row]`; a missing entry falls back to parsing the cell text.
    keys: Vec<Option<Vec<SortKey>>>,
}

impl RowModel {
    /// A model over `rows`.
    pub fn new(rows: Vec<TableRow>) -> Self {
        Self {
            rows,
            keys: Vec::new(),
        }
    }

    /// Builder: explicit sort keys for column `col`, index-aligned with the
    /// rows. A row without a key (the vector is too short) falls back to
    /// parsing the cell text.
    pub fn with_sort_keys(mut self, col: usize, keys: Vec<SortKey>) -> Self {
        if self.keys.len() <= col {
            self.keys.resize(col + 1, None);
        }
        self.keys[col] = Some(keys);
        self
    }

    /// The rows.
    pub fn rows(&self) -> &[TableRow] {
        &self.rows
    }
}

impl TableModel for RowModel {
    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn cell(&self, row: usize, col: usize) -> Cow<'_, str> {
        self.rows
            .get(row)
            .and_then(|r| r.cells.get(col))
            .map(|s| Cow::Borrowed(s.as_str()))
            .unwrap_or(Cow::Borrowed(""))
    }

    fn row_key(&self, row: usize) -> RowKey {
        RowKey(self.rows.get(row).map(|r| r.id.clone()).unwrap_or_default())
    }

    fn sort_key(&self, row: usize, col: usize) -> SortKey {
        self.keys
            .get(col)
            .and_then(|k| k.as_ref())
            .and_then(|k| k.get(row))
            .cloned()
            .unwrap_or_else(|| SortKey::parse(&self.cell(row, col)))
    }

    fn row_role(&self, row: usize) -> Option<Role> {
        self.rows.get(row).and_then(|r| r.role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    fn row(id: &str, cells: &[&str]) -> TableRow {
        TableRow::new(id, cells.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn empty_model_has_no_rows() {
        assert_eq!(RowModel::new(Vec::new()).row_count(), 0);
    }

    #[test]
    fn cells_read_back_and_out_of_range_is_empty() {
        let m = RowModel::new(vec![row("a", &["x", "y"])]);
        assert_eq!(&*m.cell(0, 0), "x");
        assert_eq!(&*m.cell(0, 1), "y");
        assert_eq!(&*m.cell(0, 9), "");
        assert_eq!(&*m.cell(9, 0), "");
    }

    #[test]
    fn row_key_is_the_stable_id() {
        let m = RowModel::new(vec![row("id-a", &["x"])]);
        assert_eq!(m.row_key(0), RowKey("id-a".into()));
    }

    #[test]
    fn default_sort_key_parses_bare_numbers() {
        let m = RowModel::new(vec![row("a", &["9"]), row("b", &["10"])]);
        assert_eq!(m.sort_key(0, 0).compare(&m.sort_key(1, 0)), Ordering::Less);
    }

    #[test]
    fn default_sort_key_falls_back_to_text() {
        let m = RowModel::new(vec![row("a", &["9 GB"]), row("b", &["10 GB"])]);
        assert_eq!(
            m.sort_key(0, 0).compare(&m.sort_key(1, 0)),
            Ordering::Greater
        );
    }

    #[test]
    fn explicit_sort_keys_override_the_text() {
        let m = RowModel::new(vec![row("a", &["1 GB"]), row("b", &["10 GB"])])
            .with_sort_keys(0, vec![SortKey::Num(1.0), SortKey::Num(10.0)]);
        assert_eq!(m.sort_key(0, 0).compare(&m.sort_key(1, 0)), Ordering::Less);
    }

    #[test]
    fn row_role_is_published_and_defaults_to_none() {
        let m = RowModel::new(vec![
            row("a", &["x"]),
            TableRow::new("b", vec!["y".to_string()]).with_role(Role::Error),
        ]);
        assert_eq!(m.row_role(0), None);
        assert_eq!(m.row_role(1), Some(Role::Error));
    }

    #[test]
    fn to_spec_carries_label_and_width() {
        let spec = to_spec(&Column::new("NAME", 20, Align::Left));
        assert_eq!(spec.label, "NAME");
        assert_eq!(spec.width, 20);
        assert_eq!(spec.align, tv::widgets::table::Align::Left);
    }

    #[test]
    fn to_spec_maps_right_alignment() {
        let spec = to_spec(&Column::new("SIZE", 8, Align::Right));
        assert_eq!(spec.align, tv::widgets::table::Align::Right);
    }

    #[test]
    fn to_spec_defaults_to_a_fixed_column() {
        let spec = to_spec(&Column::new("CMP", 3, Align::Left));
        assert_eq!(spec.flex, 0, "no stretch unless asked");
        assert_eq!(spec.min_width, 1, "the upstream shrink floor");
    }

    #[test]
    fn to_spec_carries_the_flex_weight_and_the_width_floor() {
        // The COMMAND column fills whatever the pane leaves: flex gives it
        // the spare cells, min_width keeps the "never below 12 cells" rule
        // (a narrower pane overflows and scrolls instead of squeezing it).
        let spec = to_spec(&Column::new("COMMAND", 0, Align::Left).flex(1).min_width(12));
        assert_eq!(spec.flex, 1, "the column stretches into spare cells");
        assert_eq!(spec.min_width, 12, "the readability floor travels too");
        assert_eq!(spec.width, 0, "start from scratch, then stretch");
    }
}
