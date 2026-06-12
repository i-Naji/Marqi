//! Markdown rendering: parse with comrak and produce styled ratatui lines.
//!
//! Two renderers share the theme + syntect highlighter:
//! - [`render_preview`] / [`render_block_node`] strip markers (preview blocks).
//! - [`tokenizer::highlight`] keeps markers, styling raw source (active block).

mod highlight;
mod preview;
mod theme;
pub mod tokenizer;

pub use highlight::CodeHighlighter;
pub(crate) use preview::merge_spans;
pub use preview::{
    ActiveLeaf, gfm_options, render as render_preview, render_block_node, render_block_with_hole,
    render_rows as render_preview_rows,
};
pub use theme::MarkdownTheme;
