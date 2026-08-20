//! Presentation layer shared by the interactive TUI and the plain CLI output.
//!
//! Anything that turns model data into something a human reads lives here, so
//! the two front-ends cannot drift apart the way `ui.rs` and `interactive.rs`
//! had already begun to.

pub mod bitmap;
pub mod format;
pub mod labels;
pub mod theme;

pub use bitmap::{
    BitmapView, bitmap_lines_fit, digit_canvas_lines, render_digit_canvas, render_preview,
};
pub use format::{
    fmt_bytes, fmt_count, fmt_duration, fmt_percent, fmt_rate, opt_u64, truncate, yes_no,
};
pub use labels::{PipelineState, finish_message, finish_reason_label, snapshot_state};
pub use theme::{Theme, ThemeName};
