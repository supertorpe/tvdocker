//! tvvision-rs widgets composed for tvdocker .
pub mod filter_bar;
pub mod log_view;
pub mod pane;
pub mod pty;
pub mod shared_line;
pub mod table;
pub mod text_pane;

pub use filter_bar::FilterBar;
pub use log_view::LogView;
pub use pane::{
    CompactIds, CompactRects, CompactWindow, PaneIds, PaneRects, PaneWindow, build_compact_window,
    build_pane_window, compact_rects, pane_rects,
};
pub use shared_line::SharedLine;
pub use table::{Align, Column, Table, TableRow};
pub use text_pane::{TextPane, severity_role};
