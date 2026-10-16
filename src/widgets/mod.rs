//! tvvision-rs widgets composed for tvdocker .
pub mod filter_bar;
pub mod log_view;
pub mod pane;
pub mod pty;
pub mod resource_table;
pub mod shared_line;
pub mod table;
pub mod text_pane;

pub use filter_bar::FilterBar;
pub use log_view::LogView;
pub use pane::{
    CompactIds, CompactRects, CompactWindow, PaneIds, PaneRects, PaneWindow, ScrollIds,
    ScrollRects, ScrollStatusIds, ScrollStatusRects, ScrollStatusWindow, ScrollWindow,
    build_compact_window, build_pane_window, build_scroll_status_window, build_scroll_window,
    compact_rects, pane_rects, scroll_rects, scroll_status_rects,
};
pub use resource_table::{Align, Column, RowModel, to_spec};
pub use shared_line::SharedLine;
pub use table::{Table, TableRow};
pub use text_pane::{TextPane, severity_role};
