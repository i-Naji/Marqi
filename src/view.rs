//! The hybrid (focus-mode) view.
//!
//! The document is partitioned into top-level blocks using comrak's AST — one
//! source of truth, shared with the read-mode preview. The block the cursor is
//! in is rendered **raw** — its exact source rows (taken from the raw
//! [`Layout`]) styled by the lossless [`tokenizer`] — while every other block is
//! rendered as **preview** (markers stripped). Blank gaps between blocks are
//! rendered raw too, so the cursor can sit in them.
//!
//! [`HybridView`] is a *row index*, not a row store: an ordered list of
//! segments (raw runs, preview blocks, the active block) with prefix-summed
//! row counts. Rendered rows exist only for the window [`assemble`] is asked
//! for; selection and the active-line background are applied at assembly time,
//! so cursor and selection changes never rebuild the structure. Inactive block
//! heights come from a content-keyed memo so an edit re-measures only the
//! blocks it changed.
//!
//! The cursor's byte↔column mapping always comes from the raw `Layout`,
//! independent of how tall the preview blocks render; the view only records
//! where the active run starts on screen so the cursor's screen row can be
//! derived.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, parse_document};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ropey::Rope;

use crate::layout::{DisplayLine, Layout};
use crate::markdown::{
    ActiveLeaf, CodeHighlighter, MarkdownTheme, gfm_options, merge_spans, render_block_node,
    render_block_with_hole, render_preview_rows, tokenizer,
};

pub struct HybridView {
    /// Document-ordered runs of rendered rows.
    segments: Vec<Segment>,
    /// `row_starts[i]` = first global rendered row of `segments[i]`;
    /// `row_starts[segments.len()]` = total rendered rows.
    row_starts: Vec<usize>,
    /// Full-layout row index where the active (raw) run begins.
    pub active_first_row: usize,
    /// Global rendered row where the active run begins.
    pub active_screen_row: usize,
    /// Source line range `[start, end]` of the active run (cache validity).
    pub active_lines: (usize, usize),
}

/// One run of rendered rows.
enum Segment {
    /// Raw layout rows for source lines `[start_line, end_line]`: blank/ref-def
    /// gaps, blocks whose isolated re-parse renders blank, the active-block
    /// raw fallback, and the whole document in raw view.
    Raw {
        start_line: usize,
        end_line: usize,
        first_layout_row: usize,
        rows: usize,
        /// Style the run with the markdown tokenizer (non-blank source); a
        /// pure-blank run renders unstyled, exactly as before.
        tokenize: bool,
        /// Lazily-built `(start_byte, per-byte styles)` for the run's source.
        tokens: Option<Rc<(usize, Vec<Style>)>>,
    },
    /// An inactive block rendered as preview; rows are fetched from the render
    /// cache at assembly time (and re-rendered there after an eviction).
    Block { block: SourceBlock, rows: usize },
    /// The cursor's block: preview rows around a raw hole. The hole rows are
    /// placeholders re-rendered from the layout at assembly time so selection
    /// and the active-line background stay exact.
    Active {
        lines: Vec<Line<'static>>,
        /// Absolute 1-based source line per row; hole rows pre-filled from the
        /// layout at build time.
        numbers: Vec<Option<usize>>,
        /// Segment-local row range of the raw hole.
        hole: Range<usize>,
        /// Byte range of the hole's source lines.
        hole_bytes: (usize, usize),
        /// Byte range of the whole block (coarse selection rule).
        block_bytes: (usize, usize),
        first_layout_row: usize,
        /// `(start_byte, per-byte styles)` for the hole's source.
        tokens: Rc<(usize, Vec<Style>)>,
    },
}

/// Rows rendered for one window, with their gutter labels.
pub struct Assembled {
    pub lines: Vec<Line<'static>>,
    /// 1-based source line per row, where known.
    pub numbers: Vec<Option<usize>>,
}

impl HybridView {
    /// Total rendered rows of the document.
    pub fn total_rows(&self) -> usize {
        self.row_starts.last().copied().unwrap_or(0)
    }

    /// Screen row of the cursor, given its full-layout row.
    pub fn cursor_screen_row(&self, cursor_layout_row: usize) -> usize {
        self.active_screen_row + cursor_layout_row.saturating_sub(self.active_first_row)
    }

    /// `(segment index, row offset within it)` for a global rendered row.
    fn locate(&self, row: usize) -> Option<(usize, usize)> {
        if self.segments.is_empty() {
            return None;
        }
        let seg = self
            .row_starts
            .partition_point(|start| *start <= row)
            .saturating_sub(1)
            .min(self.segments.len() - 1);
        Some((seg, row - self.row_starts[seg]))
    }

    fn push(&mut self, segment: Segment) {
        let rows = match &segment {
            Segment::Raw { rows, .. } | Segment::Block { rows, .. } => *rows,
            Segment::Active { lines, .. } => lines.len(),
        };
        self.row_starts.push(self.total_rows() + rows);
        self.segments.push(segment);
    }
}

const RENDER_CACHE_LIMIT_BYTES: usize = 4 * 1024 * 1024;
/// Entry cap for the block-height memo (~24B each, so worst case ~1.5MB).
const HEIGHTS_CAP: usize = 64 * 1024;

#[derive(Default)]
pub struct ViewCache {
    blocks: Vec<SourceBlock>,
    /// The document's link reference definitions (e.g. `[docs]: url`), one per
    /// line. Appended to every block's isolated re-parse so `[text][ref]`
    /// links resolve across blocks; pure definitions render nothing, so the
    /// appendix never adds rows.
    ref_defs: String,
    version: Option<u64>,
    rendered: HashMap<u64, CachedBlock>,
    rendered_bytes: usize,
    /// Rendered height per block, keyed like `rendered` but never cleared with
    /// it — the row index must stay computable without re-rendering evicted
    /// blocks, and only an edited block's key (and thus height) changes.
    heights: HashMap<u64, BlockHeight>,
    stats: ViewCacheStats,
}

/// Rendered size of a block: its row count, and whether every row is blank
/// (such blocks fall back to raw source display).
#[derive(Clone, Copy)]
struct BlockHeight {
    rows: u32,
    blank: bool,
}

#[derive(Default, Clone, Copy)]
pub struct ViewCacheStats {
    pub block_hits: usize,
    pub block_renders: usize,
    /// Blocks in the current partition (snapshot, not cumulative).
    pub blocks_total: usize,
    /// Bytes currently held by the rendered-block cache (snapshot).
    pub rendered_bytes: usize,
    /// Times the rendered-block cache was cleared for exceeding its budget.
    pub cache_clears: usize,
    /// Duration of the last whole-document block partition (`blocks_from_ast`).
    pub last_parse_us: u128,
}

#[derive(Clone)]
struct SourceBlock {
    start_line: usize,
    end_line: usize,
    start_byte: usize,
    end_byte: usize,
}

struct CachedBlock {
    lines: Vec<Line<'static>>,
    /// Per-row source line within the block's slice (1-based), where known.
    sources: Vec<Option<usize>>,
}

impl ViewCache {
    pub fn stats(&self) -> ViewCacheStats {
        ViewCacheStats {
            blocks_total: self.blocks.len(),
            rendered_bytes: self.rendered_bytes,
            ..self.stats
        }
    }

    fn ensure_blocks(&mut self, rope: &Rope, version: u64) {
        if self.version == Some(version) {
            return;
        }
        let started = Instant::now();
        (self.blocks, self.ref_defs) = blocks_from_ast(rope);
        self.stats.last_parse_us = started.elapsed().as_micros();
        self.version = Some(version);
    }

    fn cached_block(
        &mut self,
        rope: &Rope,
        block: &SourceBlock,
        width: usize,
        theme: &MarkdownTheme,
        highlighter: &CodeHighlighter,
    ) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
        let mut source = source_slice(rope, block.start_byte, block.end_byte);
        append_ref_defs(&mut source, &self.ref_defs);
        let key = block_cache_key(&source, width, theme.heading_glyphs, theme.hard_breaks);
        if let Some(cached) = self.rendered.get(&key) {
            self.stats.block_hits += 1;
            return (cached.lines.clone(), cached.sources.clone());
        }

        let (lines, sources): (Vec<_>, Vec<_>) =
            render_preview_rows(&source, width, theme, highlighter)
                .into_iter()
                .map(|row| (row.line, row.source))
                .unzip();
        let bytes = rendered_size(&lines);
        if self.rendered_bytes + bytes > RENDER_CACHE_LIMIT_BYTES {
            self.rendered.clear();
            self.rendered_bytes = 0;
            self.stats.cache_clears += 1;
        }
        self.rendered_bytes += bytes;
        self.record_height(key, &lines);
        self.rendered.insert(
            key,
            CachedBlock {
                lines: lines.clone(),
                sources: sources.clone(),
            },
        );
        self.stats.block_renders += 1;
        (lines, sources)
    }

    fn record_height(&mut self, key: u64, lines: &[Line<'static>]) {
        if self.heights.len() >= HEIGHTS_CAP {
            self.heights.clear();
        }
        self.heights.insert(
            key,
            BlockHeight {
                rows: lines.len() as u32,
                blank: renders_blank(lines),
            },
        );
    }

    /// Rendered height of a block, from the memo when possible; a miss renders
    /// the block once (populating the render cache too) and memoizes.
    fn block_height(
        &mut self,
        rope: &Rope,
        block: &SourceBlock,
        width: usize,
        theme: &MarkdownTheme,
        highlighter: &CodeHighlighter,
    ) -> BlockHeight {
        let mut source = source_slice(rope, block.start_byte, block.end_byte);
        append_ref_defs(&mut source, &self.ref_defs);
        let key = block_cache_key(&source, width, theme.heading_glyphs, theme.hard_breaks);
        if let Some(height) = self.heights.get(&key) {
            return *height;
        }
        let (lines, _) = self.cached_block(rope, block, width, theme, highlighter);
        let height = BlockHeight {
            rows: lines.len() as u32,
            blank: renders_blank(&lines),
        };
        self.heights.insert(key, height);
        height
    }
}

pub fn render_preview_cached(
    cache: &mut ViewCache,
    rope: &Rope,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
    version: u64,
) -> Vec<Line<'static>> {
    cache.ensure_blocks(rope, version);
    let width = width.max(1);
    let total = rope.len_lines();
    let mut out = Vec::new();
    let blocks = cache.blocks.clone();
    let mut next_line = 0usize;
    for block in &blocks {
        push_gap_defs(rope, next_line, block.start_line, theme, &mut out);
        if !out.is_empty() {
            out.push(Line::default());
        }
        let (block_lines, _) = cache.cached_block(rope, block, width, theme, highlighter);
        if renders_blank(&block_lines) {
            // The isolated re-parse consumed the whole block (e.g. a footnote
            // definition whose reference lives in another block): show its
            // tokenized source rather than a blank hole.
            let source = source_slice(rope, block.start_byte, block.end_byte);
            out.extend(tokenized_source_lines(&source, theme));
        } else {
            out.extend(block_lines);
        }
        next_line = block.end_line + 1;
    }
    push_gap_defs(rope, next_line, total, theme, &mut out);
    if out.is_empty() {
        out.push(Line::default());
    }
    out
}

/// Append a gap's non-blank content to the read preview. Gap lines are exactly
/// the link reference definitions (everything else is owned by a block), and
/// hiding them entirely — as a website renderer would — reads as data loss in
/// an editor. Leading/trailing blank lines collapse into the usual separator;
/// interior structure is kept.
fn push_gap_defs(
    rope: &Rope,
    from: usize,
    to_exclusive: usize,
    theme: &MarkdownTheme,
    out: &mut Vec<Line<'static>>,
) {
    let lines: Vec<String> = (from..to_exclusive)
        .map(|l| rope.line(l).to_string())
        .collect();
    let Some(first) = lines.iter().position(|l| !l.trim().is_empty()) else {
        return;
    };
    let last = lines.iter().rposition(|l| !l.trim().is_empty()).unwrap();
    if !out.is_empty() {
        out.push(Line::default());
    }
    for text in &lines[first..=last] {
        out.extend(tokenized_source_lines(text, theme));
    }
}

/// Raw source styled by the markdown tokenizer — for read-mode rows that have
/// no preview rendering (reference definitions, consumed blocks).
fn tokenized_source_lines(source: &str, theme: &MarkdownTheme) -> Vec<Line<'static>> {
    let styles = tokenizer::highlight(source, theme);
    let mut out = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let row = content.char_indices().map(|(i, ch)| {
            let style = styles.get(offset + i).copied().unwrap_or(theme.text);
            (ch.to_string(), style)
        });
        out.push(merge_spans(row));
        offset += line.len();
    }
    out
}

/// The pre-index full-document view, kept as the byte-exact oracle the
/// windowed [`assemble`] path is equivalence-tested against.
#[cfg(test)]
pub struct FullView {
    pub lines: Vec<Line<'static>>,
    pub line_numbers: Vec<Option<usize>>,
    pub active_first_row: usize,
    pub active_screen_row: usize,
    pub active_lines: (usize, usize),
}

#[cfg(test)]
impl FullView {
    pub fn cursor_screen_row(&self, cursor_layout_row: usize) -> usize {
        self.active_screen_row + cursor_layout_row.saturating_sub(self.active_first_row)
    }
}

/// Legacy full-document hybrid build (selection and active-line styling baked
/// in). Test-only oracle; the live path is [`build_index`] + [`assemble`].
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn build_full(
    cache: &mut ViewCache,
    rope: &Rope,
    layout: &Layout,
    cursor_byte: usize,
    selection: Option<(usize, usize)>,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
    version: u64,
) -> FullView {
    cache.ensure_blocks(rope, version);
    let width = width.max(1);
    let sel = selection.map(|s| (s, theme.selection));
    let total_lines = rope.len_lines();
    let cursor_line = rope
        .byte_to_line(cursor_byte)
        .min(total_lines.saturating_sub(1));
    let active_idx = cache
        .blocks
        .iter()
        .position(|b| (b.start_line..=b.end_line).contains(&cursor_line));

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut line_numbers: Vec<Option<usize>> = Vec::new();
    let mut active_first_row = 0;
    let mut active_screen_row = 0;
    let mut active_lines = (cursor_line, cursor_line);

    let mut line = 0;
    let blocks = cache.blocks.clone();
    let ref_defs = cache.ref_defs.clone();
    for (idx, block) in blocks.iter().enumerate() {
        if line < block.start_line {
            push_raw_gap(
                rope,
                layout,
                line,
                block.start_line - 1,
                cursor_line,
                theme,
                sel,
                &mut lines,
                &mut line_numbers,
                &mut active_first_row,
                &mut active_screen_row,
                &mut active_lines,
            );
        }

        if Some(idx) == active_idx {
            let active = render_active_cached(
                rope,
                layout,
                block,
                cursor_line,
                width,
                theme,
                highlighter,
                sel,
                &ref_defs,
            );
            active_screen_row = lines.len() + active.hole_row;
            active_first_row = active.first_layout_row;
            active_lines = active.active_lines;
            line_numbers.extend(active.line_numbers);
            lines.extend(active.lines);
        } else {
            let (mut block_lines, sources) =
                cache.cached_block(rope, block, width, theme, highlighter);
            if renders_blank(&block_lines) {
                // The isolated re-parse consumed the whole block (e.g. a
                // footnote definition whose reference lives in another block):
                // show the raw source rows rather than a blank hole.
                push_raw_gap(
                    rope,
                    layout,
                    block.start_line,
                    block.end_line,
                    cursor_line,
                    theme,
                    sel,
                    &mut lines,
                    &mut line_numbers,
                    &mut active_first_row,
                    &mut active_screen_row,
                    &mut active_lines,
                );
            } else {
                if let Some((range, color)) = sel
                    && intersects((block.start_byte, block.end_byte), range)
                {
                    highlight_block(&mut block_lines, color);
                }
                // Map block-relative (1-based) source lines to absolute ones.
                line_numbers.extend(
                    sources
                        .into_iter()
                        .map(|src| src.map(|rel| block.start_line + rel)),
                );
                lines.extend(block_lines);
            }
        }
        line = block.end_line + 1;
    }

    if line < total_lines {
        push_raw_gap(
            rope,
            layout,
            line,
            total_lines - 1,
            cursor_line,
            theme,
            sel,
            &mut lines,
            &mut line_numbers,
            &mut active_first_row,
            &mut active_screen_row,
            &mut active_lines,
        );
    }

    if lines.is_empty() {
        lines.push(Line::default());
        line_numbers.push(None);
    }

    FullView {
        lines,
        line_numbers,
        active_first_row,
        active_screen_row,
        active_lines,
    }
}

/// Build the hybrid row index: segment the document into raw runs, preview
/// blocks, and the active block, with exact per-segment row counts but no
/// rendered rows. Selection and cursor styling are assembly inputs, so this
/// only rebuilds when content, width, or the active region change.
#[allow(clippy::too_many_arguments)]
pub fn build_index(
    cache: &mut ViewCache,
    rope: &Rope,
    layout: &Layout,
    cursor_byte: usize,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
    version: u64,
) -> HybridView {
    cache.ensure_blocks(rope, version);
    let width = width.max(1);
    let total_lines = rope.len_lines();
    let cursor_line = rope
        .byte_to_line(cursor_byte)
        .min(total_lines.saturating_sub(1));
    let active_idx = cache
        .blocks
        .iter()
        .position(|b| (b.start_line..=b.end_line).contains(&cursor_line));

    let mut view = HybridView {
        segments: Vec::new(),
        row_starts: vec![0],
        active_first_row: 0,
        active_screen_row: 0,
        active_lines: (cursor_line, cursor_line),
    };

    let mut line = 0;
    let blocks = cache.blocks.clone();
    let ref_defs = cache.ref_defs.clone();
    for (idx, block) in blocks.iter().enumerate() {
        if line < block.start_line {
            push_raw_segment(
                rope,
                layout,
                line,
                block.start_line - 1,
                cursor_line,
                &mut view,
            );
        }

        if Some(idx) == active_idx {
            push_active_segment(
                rope,
                layout,
                block,
                cursor_line,
                width,
                theme,
                highlighter,
                &ref_defs,
                &mut view,
            );
        } else {
            let height = cache.block_height(rope, block, width, theme, highlighter);
            if height.blank {
                // The isolated re-parse consumed the whole block (e.g. a
                // footnote definition whose reference lives in another block):
                // show the raw source rows rather than a blank hole.
                push_raw_segment(
                    rope,
                    layout,
                    block.start_line,
                    block.end_line,
                    cursor_line,
                    &mut view,
                );
            } else {
                view.push(Segment::Block {
                    block: block.clone(),
                    rows: height.rows as usize,
                });
            }
        }
        line = block.end_line + 1;
    }

    if line < total_lines {
        push_raw_segment(rope, layout, line, total_lines - 1, cursor_line, &mut view);
    }

    debug_assert!(view.total_rows() >= 1, "a document always renders one row");
    view
}

/// Append a raw segment covering source lines `[line, end_line]`, recording
/// the active-run anchors when the cursor sits inside it.
fn push_raw_segment(
    rope: &Rope,
    layout: &Layout,
    line: usize,
    end_line: usize,
    cursor_line: usize,
    view: &mut HybridView,
) {
    let total = rope.len_lines();
    let (first_row, end_row) = row_range(rope, layout, line, end_line, total);
    let start_byte = rope.line_to_byte(line);
    let end_byte = if end_line + 1 < total {
        rope.line_to_byte(end_line + 1)
    } else {
        rope.len_bytes()
    };
    if (line..=end_line).contains(&cursor_line) {
        view.active_first_row = first_row;
        view.active_screen_row = view.total_rows();
        view.active_lines = (line, end_line);
    }
    view.push(Segment::Raw {
        start_line: line,
        end_line,
        first_layout_row: first_row,
        rows: end_row - first_row,
        tokenize: !slice_is_blank(rope, start_byte, end_byte),
        tokens: None,
    });
}

/// Append the active block: preview rows with a raw hole for the leaf under
/// the cursor. Falls back to a raw segment when the cursor sits on lines the
/// re-parse does not cover (or the block renders blank).
#[allow(clippy::too_many_arguments)]
fn push_active_segment(
    rope: &Rope,
    layout: &Layout,
    block: &SourceBlock,
    cursor_line: usize,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
    ref_defs: &str,
    view: &mut HybridView,
) {
    let mut source = source_slice(rope, block.start_byte, block.end_byte);
    append_ref_defs(&mut source, ref_defs);
    let arena = Arena::new();
    let root = parse_document(&arena, &source, &gfm_options());
    let rel_cursor = cursor_line.saturating_sub(block.start_line);
    let total = block.end_line.saturating_sub(block.start_line) + 1;

    let Some(active_child) = root
        .children()
        .find(|child| line_contains(child, rel_cursor))
    else {
        return push_raw_segment(
            rope,
            layout,
            block.start_line,
            block.end_line,
            cursor_line,
            view,
        );
    };
    let leaf_node = find_active_leaf(active_child, rel_cursor);
    let (rel_la, rel_lb) = if matches!(leaf_node.data.borrow().value, NodeValue::List(_)) {
        // The cursor is in whitespace between items of a (loose) list — there is
        // no item to open, so open only the blank run the cursor is in rather
        // than rendering the whole list raw.
        blank_run_at(&source, rel_cursor, total)
    } else {
        let sp = leaf_node.data.borrow().sourcepos;
        (
            sp.start.line.saturating_sub(1).min(total - 1),
            sp.end.line.saturating_sub(1).min(total - 1),
        )
    };
    let abs_la = block.start_line + rel_la;
    let abs_lb = block.start_line + rel_lb;
    let doc_total = rope.len_lines();
    let (first_layout_row, end_layout_row) = row_range(rope, layout, abs_la, abs_lb, doc_total);
    let raw_len = end_layout_row - first_layout_row;
    let active = ActiveLeaf::new((rel_la, rel_lb), vec![Line::default(); raw_len]);

    let mut lines = Vec::new();
    let mut sources = Vec::new();
    let mut hole_row = None;
    for child in root.children() {
        let child_rows = if line_contains(child, rel_cursor) {
            let before = lines.len();
            let (child_rows, hole) =
                render_block_with_hole(child, width, theme, highlighter, &active);
            if let Some(hole) = hole {
                hole_row = Some(before + hole);
            }
            child_rows
        } else {
            render_block_node(child, width, theme, highlighter)
        };
        for row in child_rows {
            sources.push(row.source);
            lines.push(row.line);
        }
    }

    if lines.is_empty() {
        return push_raw_segment(
            rope,
            layout,
            block.start_line,
            block.end_line,
            cursor_line,
            view,
        );
    }

    // Map block-relative (1-based) source lines to absolute ones; the hole's
    // raw rows then get exact per-row numbers from the layout.
    let mut numbers: Vec<Option<usize>> = sources
        .into_iter()
        .map(|src| src.map(|rel| block.start_line + rel))
        .collect();
    // `None` is unreachable in practice: the hole range comes from the same
    // re-parsed AST whose sourcepos the hole renderer matches, so the hole
    // always emits. Falling back to the block's first row keeps this total.
    let hole = hole_row.unwrap_or(0);
    layout.for_each_row(first_layout_row..end_layout_row, |row, dl| {
        if let Some(slot) = numbers.get_mut(hole + (row - first_layout_row)) {
            *slot = Some(dl.line + 1);
        }
    });

    let hole_start = rope.line_to_byte(abs_la);
    let hole_end = if abs_lb + 1 < doc_total {
        rope.line_to_byte(abs_lb + 1)
    } else {
        rope.len_bytes()
    };
    let hole_source = source_slice(rope, hole_start, hole_end);
    let tokens = Rc::new((hole_start, tokenizer::highlight(&hole_source, theme)));

    view.active_first_row = first_layout_row;
    view.active_screen_row = view.total_rows() + hole;
    view.active_lines = (abs_la, abs_lb);
    view.push(Segment::Active {
        lines,
        numbers,
        hole: hole..hole + raw_len,
        hole_bytes: (hole_start, hole_end),
        block_bytes: (block.start_byte, block.end_byte),
        first_layout_row,
        tokens,
    });
}

/// Whether a byte range of the rope contains only whitespace.
fn slice_is_blank(rope: &Rope, start: usize, end: usize) -> bool {
    rope.slice(rope.byte_to_char(start)..rope.byte_to_char(end))
        .chars()
        .all(char::is_whitespace)
}

/// Legacy full-document raw build (test oracle; see [`build_full`]).
#[cfg(test)]
pub fn build_raw_full(
    rope: &Rope,
    layout: &Layout,
    cursor_byte: usize,
    selection: Option<(usize, usize)>,
    theme: &MarkdownTheme,
) -> FullView {
    let total = rope.len_lines();
    let cursor_line = rope.byte_to_line(cursor_byte).min(total.saturating_sub(1));
    let sel = selection.map(|s| (s, theme.selection));
    let source = rope.to_string();
    let styles = (0usize, tokenizer::highlight(&source, theme));

    let mut lines = Vec::with_capacity(layout.len());
    let mut line_numbers = Vec::with_capacity(layout.len());
    for row in layout.rows() {
        line_numbers.push(Some(row.line + 1));
        lines.push(raw_row(
            rope,
            row,
            layout.line_start(row.line),
            Some(&styles),
            theme.text,
            Some(cursor_line),
            theme.active_line,
            sel,
        ));
    }
    if lines.is_empty() {
        lines.push(Line::default());
        line_numbers.push(None);
    }

    FullView {
        lines,
        line_numbers,
        active_first_row: 0,
        active_screen_row: 0,
        active_lines: (0, total.saturating_sub(1)),
    }
}

/// Build the raw-view row index: one raw segment covering the whole document
/// (markers kept, fully editable; screen row equals layout row). The tokenizer
/// table is built lazily on first assembly and lives for the index's lifetime
/// — one tokenize per edit/width change, instead of one per cursor move.
pub fn build_raw_index(rope: &Rope, layout: &Layout) -> HybridView {
    let total = rope.len_lines();
    let rows = layout.len();
    HybridView {
        segments: vec![Segment::Raw {
            start_line: 0,
            end_line: total.saturating_sub(1),
            first_layout_row: 0,
            rows,
            tokenize: true,
            tokens: None,
        }],
        row_starts: vec![0, rows],
        active_first_row: 0,
        active_screen_row: 0,
        active_lines: (0, total.saturating_sub(1)),
    }
}

/// Render the rows `[start_row, start_row + count)` of the index. The only
/// place rendered hybrid/raw rows are produced: selection and the active-line
/// background are applied here, per window, from the current cursor state.
#[allow(clippy::too_many_arguments)]
pub fn assemble(
    view: &mut HybridView,
    cache: &mut ViewCache,
    rope: &Rope,
    layout: &Layout,
    cursor_line: usize,
    selection: Option<(usize, usize)>,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
    start_row: usize,
    count: usize,
) -> Assembled {
    let width = width.max(1);
    let sel = selection.map(|s| (s, theme.selection));
    let total = view.total_rows();
    let start = start_row.min(total);
    let end = (start + count).min(total);
    let mut out = Assembled {
        lines: Vec::with_capacity(end - start),
        numbers: Vec::with_capacity(end - start),
    };

    let mut row = start;
    while row < end {
        let Some((seg, offset)) = view.locate(row) else {
            break;
        };
        let seg_rows = view.row_starts[seg + 1] - view.row_starts[seg];
        let take = (end - row).min(seg_rows - offset);
        match &mut view.segments[seg] {
            Segment::Raw {
                start_line,
                end_line,
                first_layout_row,
                tokenize,
                tokens,
                ..
            } => {
                if *tokenize && tokens.is_none() {
                    let total_lines = rope.len_lines();
                    let start_byte = rope.line_to_byte(*start_line);
                    let end_byte = if *end_line + 1 < total_lines {
                        rope.line_to_byte(*end_line + 1)
                    } else {
                        rope.len_bytes()
                    };
                    let source = source_slice(rope, start_byte, end_byte);
                    *tokens = Some(Rc::new((start_byte, tokenizer::highlight(&source, theme))));
                }
                let table = tokens.as_deref();
                layout.for_each_row(
                    *first_layout_row + offset..*first_layout_row + offset + take,
                    |_, dl| {
                        out.numbers.push(Some(dl.line + 1));
                        out.lines.push(raw_row(
                            rope,
                            dl,
                            layout.line_start(dl.line),
                            table,
                            theme.text,
                            Some(cursor_line),
                            theme.active_line,
                            sel,
                        ));
                    },
                );
            }
            Segment::Block { block, .. } => {
                let (mut block_lines, sources) =
                    cache.cached_block(rope, block, width, theme, highlighter);
                if let Some((range, color)) = sel
                    && intersects((block.start_byte, block.end_byte), range)
                {
                    highlight_block(&mut block_lines, color);
                }
                for (line, source) in block_lines.into_iter().zip(sources).skip(offset).take(take) {
                    out.numbers.push(source.map(|rel| block.start_line + rel));
                    out.lines.push(line);
                }
            }
            Segment::Active {
                lines,
                numbers,
                hole,
                hole_bytes,
                block_bytes,
                first_layout_row,
                tokens,
            } => {
                // Coarse selection highlight for the preview rows around the
                // hole, only when the selection actually reaches outside the
                // hole's byte range (the hole rows carry exact per-byte
                // selection from `raw_row`).
                let coarse = sel.and_then(|(range, color)| {
                    (intersects(*block_bytes, range)
                        && (range.0 < hole_bytes.0 || range.1 > hole_bytes.1))
                        .then_some(color)
                });
                for r in offset..offset + take {
                    out.numbers.push(numbers[r]);
                    if hole.contains(&r) {
                        layout.with_row(*first_layout_row + (r - hole.start), |dl| {
                            out.lines.push(raw_row(
                                rope,
                                dl,
                                layout.line_start(dl.line),
                                Some(tokens),
                                theme.text,
                                Some(cursor_line),
                                theme.active_line,
                                sel,
                            ));
                        });
                    } else {
                        let mut line = lines[r].clone();
                        if let Some(color) = coarse {
                            highlight_block(std::slice::from_mut(&mut line), color);
                        }
                        out.lines.push(line);
                    }
                }
            }
        }
        row += take;
    }
    out
}

/// Where a rendered row points in the source, for mouse mapping.
pub enum RowTarget {
    /// The row maps 1:1 onto this layout row — `pos_to_byte` is exact.
    LayoutRow(usize),
    /// A preview row: best-known 0-based source line, label-estimated within
    /// its segment (never crossing into a neighbouring label's line).
    SourceLine(usize),
}

/// Resolve a rendered row for mouse mapping. Raw runs and the active hole map
/// exactly onto layout rows; preview rows estimate from their segment's
/// labels, falling back to the block's first line.
pub fn row_target(
    view: &HybridView,
    cache: &mut ViewCache,
    rope: &Rope,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
    row: usize,
) -> Option<RowTarget> {
    let (seg, offset) = view.locate(row)?;
    let line_1based = match &view.segments[seg] {
        Segment::Raw {
            first_layout_row, ..
        } => return Some(RowTarget::LayoutRow(first_layout_row + offset)),
        Segment::Active {
            numbers,
            hole,
            first_layout_row,
            ..
        } => {
            if hole.contains(&offset) {
                return Some(RowTarget::LayoutRow(
                    first_layout_row + (offset - hole.start),
                ));
            }
            label_estimate(numbers, offset)
        }
        Segment::Block { block, .. } => {
            let (_, sources) = cache.cached_block(rope, block, width.max(1), theme, highlighter);
            let numbers: Vec<Option<usize>> = sources
                .into_iter()
                .map(|rel| rel.map(|r| block.start_line + r))
                .collect();
            label_estimate(&numbers, offset).or(Some(block.start_line + 1))
        }
    }?;
    Some(RowTarget::SourceLine(line_1based.saturating_sub(1)))
}

/// Estimate a row's 1-based source line from the nearest labeled row in its
/// segment: nearest label at-or-above plus the row distance, clamped below the
/// next label; with nothing above, count back from the first label below.
fn label_estimate(numbers: &[Option<usize>], offset: usize) -> Option<usize> {
    if let Some((base_row, base_line)) = (0..=offset)
        .rev()
        .find_map(|r| numbers.get(r).copied().flatten().map(|l| (r, l)))
    {
        let mut line = base_line + (offset - base_row);
        if let Some(next) =
            (offset + 1..numbers.len()).find_map(|r| numbers.get(r).copied().flatten())
        {
            line = line.min(next.saturating_sub(1)).max(base_line);
        }
        Some(line)
    } else {
        let (below_row, below_line) = (offset + 1..numbers.len())
            .find_map(|r| numbers.get(r).copied().flatten().map(|l| (r, l)))?;
        Some((below_line + offset).saturating_sub(below_row).max(1))
    }
}

#[cfg(test)]
struct ActiveCachedRender {
    lines: Vec<Line<'static>>,
    line_numbers: Vec<Option<usize>>,
    first_layout_row: usize,
    hole_row: usize,
    active_lines: (usize, usize),
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn render_active_cached(
    rope: &Rope,
    layout: &Layout,
    block: &SourceBlock,
    cursor_line: usize,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
    sel: Option<((usize, usize), Color)>,
    ref_defs: &str,
) -> ActiveCachedRender {
    let mut source = source_slice(rope, block.start_byte, block.end_byte);
    append_ref_defs(&mut source, ref_defs);
    let arena = Arena::new();
    let root = parse_document(&arena, &source, &gfm_options());
    let rel_cursor = cursor_line.saturating_sub(block.start_line);
    let total = block.end_line.saturating_sub(block.start_line) + 1;

    let Some(active_child) = root
        .children()
        .find(|child| line_contains(child, rel_cursor))
    else {
        return raw_block_cached(rope, layout, block, cursor_line, theme, sel);
    };
    let leaf_node = find_active_leaf(active_child, rel_cursor);
    let (rel_la, rel_lb) = if matches!(leaf_node.data.borrow().value, NodeValue::List(_)) {
        // The cursor is in whitespace between items of a (loose) list — there is
        // no item to open, so open only the blank run the cursor is in rather
        // than rendering the whole list raw.
        blank_run_at(&source, rel_cursor, total)
    } else {
        let sp = leaf_node.data.borrow().sourcepos;
        (
            sp.start.line.saturating_sub(1).min(total - 1),
            sp.end.line.saturating_sub(1).min(total - 1),
        )
    };
    let abs_la = block.start_line + rel_la;
    let abs_lb = block.start_line + rel_lb;
    let (raw, first_layout_row) = raw_lines(rope, layout, abs_la, abs_lb, cursor_line, theme, sel);
    let raw_len = raw.len();
    let active = ActiveLeaf::new((rel_la, rel_lb), raw);

    let mut lines = Vec::new();
    let mut sources = Vec::new();
    let mut hole_row = None;
    for child in root.children() {
        let child_rows = if line_contains(child, rel_cursor) {
            let before = lines.len();
            let (child_rows, hole) =
                render_block_with_hole(child, width, theme, highlighter, &active);
            if let Some(hole) = hole {
                hole_row = Some(before + hole);
            }
            child_rows
        } else {
            render_block_node(child, width, theme, highlighter)
        };
        for row in child_rows {
            sources.push(row.source);
            lines.push(row.line);
        }
    }

    if lines.is_empty() {
        return raw_block_cached(rope, layout, block, cursor_line, theme, sel);
    }

    // Selection highlight for the *preview-rendered* rows of the active block
    // (e.g. sibling list items around the raw hole). The hole rows already
    // carry exact per-byte selection from `raw_lines`; the preview rows get
    // the same coarse block highlight as inactive blocks, but only when the
    // selection actually reaches outside the hole's byte range.
    if let Some((range, color)) = sel
        && intersects((block.start_byte, block.end_byte), range)
    {
        let total_lines = rope.len_lines();
        let hole_start = rope.line_to_byte(abs_la);
        let hole_end = if abs_lb + 1 < total_lines {
            rope.line_to_byte(abs_lb + 1)
        } else {
            rope.len_bytes()
        };
        if range.0 < hole_start || range.1 > hole_end {
            let hole = hole_row.map(|h| h..h + raw_len).unwrap_or(0..0);
            for (i, line) in lines.iter_mut().enumerate() {
                if !hole.contains(&i) {
                    highlight_block(std::slice::from_mut(line), color);
                }
            }
        }
    }

    // Map block-relative (1-based) source lines to absolute ones; the hole's
    // raw rows then get exact per-row numbers from the full layout.
    let mut line_numbers: Vec<Option<usize>> = sources
        .into_iter()
        .map(|src| src.map(|rel| block.start_line + rel))
        .collect();
    if let Some(hole) = hole_row {
        for (offset, row) in layout.rows()[first_layout_row..first_layout_row + raw_len]
            .iter()
            .enumerate()
        {
            if let Some(slot) = line_numbers.get_mut(hole + offset) {
                *slot = Some(row.line + 1);
            }
        }
    }

    ActiveCachedRender {
        lines,
        line_numbers,
        first_layout_row,
        // `None` is unreachable in practice: the hole range comes from the
        // same re-parsed AST whose sourcepos `try_emit_raw_range` matches, so
        // the hole always emits. Falling back to the block's first row keeps
        // this total rather than panicking.
        hole_row: hole_row.unwrap_or(0),
        active_lines: (abs_la, abs_lb),
    }
}

#[cfg(test)]
fn raw_block_cached(
    rope: &Rope,
    layout: &Layout,
    block: &SourceBlock,
    cursor_line: usize,
    theme: &MarkdownTheme,
    sel: Option<((usize, usize), Color)>,
) -> ActiveCachedRender {
    let (lines, first_layout_row) = raw_lines(
        rope,
        layout,
        block.start_line,
        block.end_line,
        cursor_line,
        theme,
        sel,
    );
    let line_numbers = layout.rows()[first_layout_row..first_layout_row + lines.len()]
        .iter()
        .map(|row| Some(row.line + 1))
        .collect();
    ActiveCachedRender {
        lines,
        line_numbers,
        first_layout_row,
        hole_row: 0,
        active_lines: (block.start_line, block.end_line),
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn push_raw_gap(
    rope: &Rope,
    layout: &Layout,
    line: usize,
    end_line: usize,
    cursor_line: usize,
    theme: &MarkdownTheme,
    sel: Option<((usize, usize), Color)>,
    lines: &mut Vec<Line<'static>>,
    line_numbers: &mut Vec<Option<usize>>,
    active_first_row: &mut usize,
    active_screen_row: &mut usize,
    active_lines: &mut (usize, usize),
) {
    let total = rope.len_lines();
    let contains_cursor = (line..=end_line).contains(&cursor_line);
    let (first_row, end_row) = row_range(rope, layout, line, end_line, total);
    let screen_offset = lines.len();
    // Tokenize the gap so its non-blank lines (link reference definitions)
    // read as raw markdown rather than flat text. Most gaps are pure blank
    // lines, which skip the tokenizer entirely.
    let start_byte = rope.line_to_byte(line);
    let end_byte = if end_line + 1 < total {
        rope.line_to_byte(end_line + 1)
    } else {
        rope.len_bytes()
    };
    let gap_source = source_slice(rope, start_byte, end_byte);
    let table = (!gap_source.trim().is_empty())
        .then(|| (start_byte, tokenizer::highlight(&gap_source, theme)));
    for row in &layout.rows()[first_row..end_row] {
        line_numbers.push(Some(row.line + 1));
        lines.push(raw_row(
            rope,
            row,
            layout.line_start(row.line),
            table.as_ref(),
            theme.text,
            Some(cursor_line),
            theme.active_line,
            sel,
        ));
    }
    if contains_cursor {
        *active_first_row = first_row;
        *active_screen_row = screen_offset;
        *active_lines = (line, end_line);
    }
}

fn blocks_from_ast(rope: &Rope) -> (Vec<SourceBlock>, String) {
    let total = rope.len_lines();
    if total == 0 {
        return (Vec::new(), String::new());
    }
    // comrak's parse is the single source of truth for block boundaries, so the
    // hybrid view and read-mode preview agree and constructs like setext
    // headings, indented code, and loose lists are grouped exactly as rendered.
    let source = rope.to_string();
    let arena = Arena::new();
    let root = parse_document(&arena, &source, &gfm_options());

    // Map each line to its owning top-level child, then group maximal same-owner
    // runs into blocks. Unowned lines are blank gaps the caller renders raw.
    let mut owner: Vec<Option<usize>> = vec![None; total];
    for (idx, node) in root.children().enumerate() {
        let sp = node.data.borrow().sourcepos;
        let start = sp.start.line.saturating_sub(1).min(total - 1);
        let end = sp.end.line.saturating_sub(1).min(total - 1);
        for slot in owner.iter_mut().take(end + 1).skip(start) {
            *slot = Some(idx);
        }
    }

    let mut blocks = Vec::new();
    let mut line = 0;
    while line < total {
        let Some(idx) = owner[line] else {
            line += 1;
            continue;
        };
        let start = line;
        while line < total && owner[line] == Some(idx) {
            line += 1;
        }
        let end = line - 1;
        let start_byte = rope.line_to_byte(start);
        let end_byte = if end + 1 < total {
            rope.line_to_byte(end + 1)
        } else {
            rope.len_bytes()
        };
        blocks.push(SourceBlock {
            start_line: start,
            end_line: end,
            start_byte,
            end_byte,
        });
    }
    (blocks, ref_defs_from_gaps(rope, &owner))
}

/// Collect the document's link reference definitions, one per line.
///
/// Definitions never appear in the AST — comrak consumes them into its
/// refmap — so their lines are exactly the non-blank *unowned* ones. A line is
/// taken only if it alone re-parses to an empty document, the signature of a
/// pure definition (a look-alike inside a paragraph or code block is owned and
/// never reaches the check). Footnote definitions are skipped: appending one
/// would make it render inside any block that references it.
fn ref_defs_from_gaps(rope: &Rope, owner: &[Option<usize>]) -> String {
    // comrak itself decides what qualifies. Footnotes are disabled for the
    // probe so a `[^name]:` definition parses as a paragraph and is rejected —
    // appending one would make it render inside any block referencing it.
    let mut probe_options = gfm_options();
    probe_options.extension.footnotes = false;
    let mut out = String::new();
    for (idx, owned) in owner.iter().enumerate() {
        if owned.is_some() {
            continue;
        }
        let line = rope.line(idx).to_string();
        if line.trim().is_empty() {
            continue;
        }
        let arena = Arena::new();
        if parse_document(&arena, &line, &probe_options)
            .children()
            .next()
            .is_none()
        {
            out.push_str(line.trim());
            out.push('\n');
        }
    }
    out
}

/// Append the document's reference definitions to a block slice about to be
/// re-parsed, separated by a blank line so they cannot lazily continue a
/// trailing paragraph.
fn append_ref_defs(source: &mut String, ref_defs: &str) {
    if ref_defs.is_empty() {
        return;
    }
    if !source.ends_with('\n') {
        source.push('\n');
    }
    source.push('\n');
    source.push_str(ref_defs);
}

/// Whether every row is visually empty — the signature of a slice re-parse
/// that consumed the whole block (e.g. an unreferenced footnote definition).
fn renders_blank(lines: &[Line<'static>]) -> bool {
    lines
        .iter()
        .all(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
}

fn source_slice(rope: &Rope, start: usize, end: usize) -> String {
    rope.slice(rope.byte_to_char(start)..rope.byte_to_char(end))
        .to_string()
}

/// Cache key for a rendered preview block.
///
/// Validity assumes the `theme` colours (beyond `heading_glyphs`) and the
/// `CodeHighlighter` are immutable for the cache's lifetime: `App` sets both
/// once in `with_config` and never mutates them, and `ViewCache` is not reset on
/// a theme change. If runtime theme/syntax switching is added, mix a
/// theme/highlighter version into this key, or clear `ViewCache` on replacement.
fn block_cache_key(source: &str, width: usize, heading_glyphs: bool, hard_breaks: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    width.hash(&mut hasher);
    heading_glyphs.hash(&mut hasher);
    hard_breaks.hash(&mut hasher);
    hasher.finish()
}

fn rendered_size(lines: &[Line<'static>]) -> usize {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.len())
        .sum()
}

/// The block to render raw: for a top-level list, the *direct* item the cursor
/// is in (siblings stay rendered); for any other block, the block itself.
///
/// We deliberately do not descend into nested sub-lists. A nested item is
/// rendered raw inside its parent item — which the list renderer would then
/// prefix with an indent, shifting the raw text and breaking the cursor's
/// column/row mapping. Opening the whole direct item keeps the raw rows
/// un-prefixed so the cursor maps exactly.
fn find_active_leaf<'a>(node: &'a AstNode<'a>, cursor_line: usize) -> &'a AstNode<'a> {
    if matches!(node.data.borrow().value, NodeValue::List(_)) {
        for item in node.children() {
            if line_contains(item, cursor_line) {
                return item;
            }
        }
    }
    node
}

/// The maximal run of blank source lines containing `cursor` (0-based) within a
/// `total`-line block — the active region when the cursor sits in the
/// whitespace between two list items.
fn blank_run_at(source: &str, cursor: usize, total: usize) -> (usize, usize) {
    let lines: Vec<&str> = source.split('\n').collect();
    let is_blank = |line: usize| lines.get(line).is_none_or(|l| l.trim().is_empty());
    let mut la = cursor;
    while la > 0 && is_blank(la - 1) {
        la -= 1;
    }
    let mut lb = cursor;
    while lb + 1 < total && is_blank(lb + 1) {
        lb += 1;
    }
    (la, lb)
}

fn line_contains(node: &AstNode, line: usize) -> bool {
    let sp = node.data.borrow().sourcepos;
    (sp.start.line.saturating_sub(1)..=sp.end.line.saturating_sub(1)).contains(&line)
}

/// Full-layout row span `[first, end)` covering source lines `[line, end_line]`.
fn row_range(
    rope: &Rope,
    layout: &Layout,
    line: usize,
    end_line: usize,
    total: usize,
) -> (usize, usize) {
    let first = layout.byte_to_pos(rope.line_to_byte(line)).0;
    let end = if end_line + 1 < total {
        layout.byte_to_pos(rope.line_to_byte(end_line + 1)).0
    } else {
        layout.len()
    };
    (first, end)
}

/// Render source lines `[la, lb]` as raw, tokenized rows; also return the
/// first full-layout row so the cursor can be positioned.
#[cfg(test)]
fn raw_lines(
    rope: &Rope,
    layout: &Layout,
    la: usize,
    lb: usize,
    cursor_line: usize,
    theme: &MarkdownTheme,
    sel: Option<((usize, usize), Color)>,
) -> (Vec<Line<'static>>, usize) {
    let total = rope.len_lines();
    let (first_row, end_row) = row_range(rope, layout, la, lb, total);
    let start_byte = rope.line_to_byte(la);
    let end_byte = if lb + 1 < total {
        rope.line_to_byte(lb + 1)
    } else {
        rope.len_bytes()
    };
    let raw_source = rope
        .slice(rope.byte_to_char(start_byte)..rope.byte_to_char(end_byte))
        .to_string();
    let table = (start_byte, tokenizer::highlight(&raw_source, theme));
    let raw = layout.rows()[first_row..end_row]
        .iter()
        .map(|row| {
            raw_row(
                rope,
                row,
                layout.line_start(row.line),
                Some(&table),
                theme.text,
                Some(cursor_line),
                theme.active_line,
                sel,
            )
        })
        .collect();
    (raw, first_row)
}

/// Whether two byte ranges overlap.
fn intersects(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Paint a selection background over every span of `lines` (block-level
/// highlight for selected preview blocks).
fn highlight_block(lines: &mut [Line<'static>], color: Color) {
    for line in lines {
        for span in &mut line.spans {
            span.style = span.style.bg(color);
        }
    }
}

/// Render one raw display row: each cell styled by its byte (active block) or
/// with the default style (blank gap), with selected cells given a background.
/// Cell text comes from slicing the source line (cells store no text); a tab
/// renders as its display width in spaces.
#[allow(clippy::too_many_arguments)]
fn raw_row(
    rope: &Rope,
    row: &DisplayLine,
    line_start: usize,
    styles: Option<&(usize, Vec<Style>)>,
    default: Style,
    active_line: Option<usize>,
    active_bg: Color,
    sel: Option<((usize, usize), Color)>,
) -> Line<'static> {
    let text = rope.line(row.line).to_string();
    let mut items: Vec<(String, Style)> = Vec::with_capacity(row.cells.len());
    for cell in &row.cells {
        let byte = line_start + cell.byte();
        let mut style = match styles {
            Some((start, table)) => table.get(byte - start).copied().unwrap_or(default),
            None => default,
        };
        if active_line == Some(row.line) {
            style = style.bg(active_bg);
        }
        if let Some(((s, e), color)) = sel
            && (s..e).contains(&byte)
        {
            style = style.bg(color);
        }
        let cluster = &text[cell.byte()..cell.byte_end()];
        let display = if cluster == "\t" {
            " ".repeat(cell.width as usize)
        } else {
            cluster.to_string()
        };
        items.push((display, style));
    }
    merge_spans(items.into_iter())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout;

    /// Build the index and assemble every row — the live pipeline shaped like
    /// the old full view, so assertions read naturally.
    #[allow(clippy::too_many_arguments)]
    fn assemble_full(
        cache: &mut ViewCache,
        rope: &Rope,
        layout: &Layout,
        cursor_byte: usize,
        selection: Option<(usize, usize)>,
        width: usize,
        theme: &MarkdownTheme,
        highlighter: &CodeHighlighter,
        version: u64,
    ) -> FullView {
        let mut view = build_index(
            cache,
            rope,
            layout,
            cursor_byte,
            width,
            theme,
            highlighter,
            version,
        );
        let cursor_line = rope
            .byte_to_line(cursor_byte)
            .min(rope.len_lines().saturating_sub(1));
        let total = view.total_rows();
        let out = assemble(
            &mut view,
            cache,
            rope,
            layout,
            cursor_line,
            selection,
            width,
            theme,
            highlighter,
            0,
            total,
        );
        FullView {
            lines: out.lines,
            line_numbers: out.numbers,
            active_first_row: view.active_first_row,
            active_screen_row: view.active_screen_row,
            active_lines: view.active_lines,
        }
    }

    fn view_for(
        rope: &Rope,
        layout: &Layout,
        cursor_byte: usize,
        selection: Option<(usize, usize)>,
        width: usize,
        theme: &MarkdownTheme,
        highlighter: &CodeHighlighter,
    ) -> FullView {
        let mut cache = ViewCache::default();
        assemble_full(
            &mut cache,
            rope,
            layout,
            cursor_byte,
            selection,
            width,
            theme,
            highlighter,
            0,
        )
    }

    fn build_for(src: &str, cursor_byte: usize) -> Vec<String> {
        let rope = Rope::from_str(src);
        let layout = Layout::build(&rope, 40, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let view = view_for(&rope, &layout, cursor_byte, None, 40, &theme, &hl);
        view.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn cursor_block_is_raw_others_are_preview() {
        let src = "# Title\n\nA para with **bold** text.\n";

        // Cursor in the heading: heading shows raw '#', paragraph is preview.
        let in_heading = build_for(src, 0).join("\n");
        assert!(
            in_heading.contains("# Title"),
            "heading should be raw:\n{in_heading}"
        );
        assert!(
            !in_heading.contains("**bold**"),
            "paragraph should be preview:\n{in_heading}"
        );

        // Cursor in the paragraph: paragraph shows raw '**', heading is preview.
        let para = src.find("A para").unwrap();
        let in_para = build_for(src, para).join("\n");
        assert!(
            in_para.contains("**bold**"),
            "paragraph should be raw:\n{in_para}"
        );
        assert!(
            !in_para.contains("# Title"),
            "heading should be preview:\n{in_para}"
        );
    }

    #[test]
    fn only_the_active_list_item_opens_raw() {
        let src = "- one with **a**\n- two with **b**\n- three with **c**\n";
        let cursor = src.find("two").unwrap();
        let out = build_for(src, cursor);
        let text = out.join("\n");

        // The active item shows its raw '- ' marker and raw '**'.
        let active = out.iter().find(|l| l.contains("two")).unwrap();
        assert!(
            active.starts_with("- two with **b**"),
            "active item raw: {active:?}"
        );

        // Siblings stay rendered: bullet glyph, markers stripped.
        let sibling = out.iter().find(|l| l.contains("one")).unwrap();
        assert!(
            sibling.contains('\u{2022}'),
            "sibling keeps bullet: {sibling:?}"
        );
        assert!(!text.contains("**a**"), "sibling markers stripped:\n{text}");
        assert!(!text.contains("**c**"), "sibling markers stripped:\n{text}");
    }

    #[test]
    fn loose_ordered_list_auto_numbers_across_blank_lines() {
        // A loose (blank-separated) list that repeats "1." must render as 1./2./3.
        // The AST partition keeps it a single block so comrak auto-numbers it
        // (a per-item split would restart every item at 1). Cursor sits in the
        // trailing paragraph so the whole list previews.
        let src = "1. one\n\n1. two\n\n1. three\n\ntail\n";
        let cursor = src.find("tail").unwrap();
        let out = build_for(src, cursor).join("\n");
        assert!(
            out.contains("2."),
            "second item should auto-number to 2.:\n{out}"
        );
        assert!(
            out.contains("3."),
            "third item should auto-number to 3.:\n{out}"
        );
    }

    #[test]
    fn selection_highlights_across_lines_in_active_block() {
        // "abc\ndef" is a single paragraph (soft break), so both lines are the
        // active raw block.
        let src = "abc\ndef\n";
        let rope = Rope::from_str(src);
        let layout = Layout::build(&rope, 40, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        // Select bytes 1..6 ("bc\nde"), cursor at 6.
        let view = view_for(&rope, &layout, 6, Some((1, 6)), 40, &theme, &hl);

        let highlighted_rows = view
            .lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.style.bg == Some(theme.selection)))
            .count();
        assert!(
            highlighted_rows >= 2,
            "multi-line selection should highlight 2+ rows, got {highlighted_rows}"
        );
    }

    #[test]
    fn selection_highlights_across_blocks() {
        // Two paragraphs; select from para 1 into para 2 (cursor in para 2).
        let src = "para one\n\npara two\n";
        let rope = Rope::from_str(src);
        let layout = Layout::build(&rope, 40, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let anchor = src.find("one").unwrap();
        let cursor = src.find("two").unwrap() + 1; // inside para 2
        let view = view_for(
            &rope,
            &layout,
            cursor,
            Some((anchor, cursor)),
            40,
            &theme,
            &hl,
        );

        let highlighted = view
            .lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.style.bg == Some(theme.selection)))
            .count();
        assert!(
            highlighted >= 2,
            "both blocks should show selection, got {highlighted}"
        );
    }

    #[test]
    fn selection_covers_preview_siblings_of_the_active_block() {
        // Select-all over a list with the cursor inside item "b": items "a"
        // and "c" render as preview rows of the *active* block and must still
        // show the selection background.
        let src = "- a\n- b\n- c\n";
        let rope = Rope::from_str(src);
        let layout = Layout::build(&rope, 40, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let cursor = src.find('b').unwrap();
        let view = view_for(
            &rope,
            &layout,
            cursor,
            Some((0, src.len())),
            40,
            &theme,
            &hl,
        );

        let highlighted = view
            .lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.style.bg == Some(theme.selection)))
            .count();
        assert!(
            highlighted >= 3,
            "all three items should show selection, got {highlighted}"
        );
    }

    #[test]
    fn nested_item_cursor_maps_without_extra_indent() {
        let src = "- outer item\n  - nested item with **x**\n";
        let cursor = src.find("nested").unwrap();
        let rope = Rope::from_str(src);
        let layout = Layout::build(&rope, 40, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let view = view_for(&rope, &layout, cursor, None, 40, &theme, &hl);
        let lines: Vec<String> = view
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let text = lines.join("\n");

        // The active item opens raw with its exact source indentation; the
        // hole replaces the item inside its parent, so the parent's indent
        // must not be applied a second time.
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("  - nested item with **x**")),
            "nested line should be raw at its source indent:\n{text}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("    - nested")),
            "nested line must not be double-indented:\n{text}"
        );

        // The cursor's screen row lands on the line it is actually in.
        let (layout_row, _) = layout.byte_to_pos(cursor);
        let screen_row = view.cursor_screen_row(layout_row);
        assert!(
            lines[screen_row].contains("nested"),
            "cursor row {screen_row} should contain 'nested', got {:?}",
            lines.get(screen_row)
        );
    }

    #[test]
    fn inactive_list_and_table_rows_get_absolute_line_numbers() {
        // Lines (1-based): 1 "intro", 2 blank, 3-4 list items, 5 blank,
        // 6 header, 7 delimiter, 8 data row. Cursor stays in "intro" so the
        // list and table render as inactive preview blocks.
        let src = "intro\n\n- a\n- b\n\n| H |\n| - |\n| d |\n";
        let rope = Rope::from_str(src);
        let layout = Layout::build(&rope, 40, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let view = view_for(&rope, &layout, 0, None, 40, &theme, &hl);

        let labeled: Vec<usize> = view.line_numbers.iter().flatten().copied().collect();
        for expect in [1, 3, 4, 6, 7, 8] {
            assert!(
                labeled.contains(&expect),
                "line {expect} should appear in {labeled:?}"
            );
        }
        // Table borders are chrome: no row may claim a line past the source.
        assert!(
            labeled.iter().all(|l| *l <= rope.len_lines()),
            "labels must stay within the document: {labeled:?}"
        );
    }

    #[test]
    fn footnote_definition_shows_raw_instead_of_vanishing() {
        // comrak drops a footnote definition whose reference lives in another
        // block, so the isolated re-parse renders nothing; the hybrid view
        // must fall back to the raw source rather than a blank row.
        let src = "A footnote[^n].\n\n[^n]: the definition body\n";
        let rope = Rope::from_str(src);
        let layout = Layout::build(&rope, 60, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let view = view_for(&rope, &layout, 0, None, 60, &theme, &hl);

        let text: Vec<String> = view
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let def_row = text
            .iter()
            .position(|l| l.contains("[^n]: the definition body"))
            .unwrap_or_else(|| panic!("definition must stay visible: {text:?}"));
        assert_eq!(
            view.line_numbers[def_row],
            Some(3),
            "the raw fallback keeps the exact line number"
        );
    }

    #[test]
    fn reference_links_resolve_across_blocks() {
        // The `[docs]:` definition lives outside the paragraph's block; the
        // appendix lets the isolated re-parse resolve it. Cursor sits on the
        // definition line (a raw gap), so the paragraph renders as preview.
        let src = "see [the docs][docs] here\n\n[docs]: https://example.com\n";
        let rope = Rope::from_str(src);
        let layout = Layout::build(&rope, 60, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let cursor = src.find("[docs]:").unwrap();
        let view = view_for(&rope, &layout, cursor, None, 60, &theme, &hl);

        let text = view
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("see the docs here"),
            "the reference link should render resolved:\n{text}"
        );
        assert!(
            !text.contains("[the docs][docs]"),
            "the raw reference must not leak into the preview:\n{text}"
        );
    }

    #[test]
    fn read_preview_shows_reference_definitions() {
        let src = "see [the docs][docs] here\n\n[docs]: https://example.com\n";
        let rope = Rope::from_str(src);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let mut cache = ViewCache::default();
        let text = render_preview_cached(&mut cache, &rope, 60, &theme, &hl, 0)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("[docs]: https://example.com"),
            "the definition should stay visible in the read preview:\n{text}"
        );
        assert!(
            text.contains("see the docs here"),
            "and the link it defines should render resolved:\n{text}"
        );
    }

    #[test]
    fn raw_fallback_rows_are_tokenized_not_flat() {
        // The footnote-definition fallback and gap lines must carry the raw
        // tokenizer's styling (link-colored label), not flat text.
        let src = "A footnote[^n].\n\n[^n]: the definition body\n";
        let rope = Rope::from_str(src);
        let layout = Layout::build(&rope, 60, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let view = view_for(&rope, &layout, 0, None, 60, &theme, &hl);

        let def_row = view
            .lines
            .iter()
            .find(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("[^n]:")
            })
            .expect("definition row present");
        assert!(
            def_row.spans.iter().any(|s| s.style == theme.link),
            "the [^n] label should use the link style: {def_row:?}"
        );
    }

    #[test]
    fn read_preview_keeps_footnote_definition_visible() {
        let src = "A footnote[^n].\n\n[^n]: the definition body\n";
        let rope = Rope::from_str(src);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let mut cache = ViewCache::default();
        let text = render_preview_cached(&mut cache, &rope, 60, &theme, &hl, 0)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("[^n]: the definition body"),
            "definition should fall back to source:\n{text}"
        );
    }

    #[test]
    fn cached_view_reuses_unchanged_inactive_blocks() {
        let src = "# One\n\nTwo\n\nThree\n";
        let rope = Rope::from_str(src);
        let layout = Layout::build(&rope, 40, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let mut cache = ViewCache::default();

        assemble_full(&mut cache, &rope, &layout, 0, None, 40, &theme, &hl, 0);
        let first = cache.stats();
        assert!(
            first.block_renders >= 2,
            "inactive blocks should render once"
        );

        let edited = Rope::from_str("# One!\n\nTwo\n\nThree\n");
        let edited_layout = Layout::build(&edited, 40, 4);
        assemble_full(
            &mut cache,
            &edited,
            &edited_layout,
            0,
            None,
            40,
            &theme,
            &hl,
            1,
        );
        let second = cache.stats();
        assert_eq!(
            second.block_renders, first.block_renders,
            "an edit to the active block re-renders nothing else"
        );
        assert!(
            second.block_hits >= first.block_hits + 2,
            "unchanged inactive blocks should be cache hits"
        );
    }

    /// The structural gate for the index rework: over a corpus of documents,
    /// cursor positions, and selections, the windowed pipeline assembled in
    /// full must be byte-identical to the legacy full build — rows, gutter
    /// labels, and the active-run anchors.
    #[test]
    fn assembled_index_matches_the_legacy_full_build() {
        use crate::testdoc;

        let mut docs = vec![
            testdoc::many_blocks(30),
            testdoc::cjk_emoji(12),
            testdoc::tabs_and_wrap(12),
            testdoc::giant_block(2048),
            "[ref]: https://example.com\n\nUse [a][ref] link.\n\nNote[^n].\n\n[^n]: def\n"
                .to_string(),
            "Setext\n===\n\npara\n\n| a | b |\n| - | - |\n| 1 | 2 |\n".to_string(),
            "- one\n\n- two\n\n- three\n".to_string(), // loose list with gaps
            "\n\n\n".to_string(),                      // blank document
            String::new(),                             // empty document
        ];
        let mut rng = testdoc::XorShift::new(0x5EED);
        docs.push(testdoc::random_doc(&mut rng, 60));

        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        for (doc_idx, src) in docs.iter().enumerate() {
            let rope = Rope::from_str(src);
            let layout = Layout::build(&rope, 40, 4);
            let len = rope.len_bytes();
            let cursors = [
                0,
                len / 3,
                len / 2,
                len.saturating_sub(1),
                len, // end-of-buffer position
            ];
            let selections = [
                None,
                Some((0, len.min(5))),
                Some((len / 4, (len / 2).max(len / 4 + 1).min(len))),
                Some((0, len)),
            ];
            for &cursor in &cursors {
                for &selection in &selections {
                    let oracle = {
                        let mut cache = ViewCache::default();
                        build_full(
                            &mut cache, &rope, &layout, cursor, selection, 40, &theme, &hl, 0,
                        )
                    };
                    let ours = {
                        let mut cache = ViewCache::default();
                        assemble_full(
                            &mut cache, &rope, &layout, cursor, selection, 40, &theme, &hl, 0,
                        )
                    };
                    let context = format!("doc {doc_idx}, cursor {cursor}, sel {selection:?}");
                    assert_eq!(
                        ours.lines.len(),
                        oracle.lines.len(),
                        "row count ({context})"
                    );
                    for (row, (a, b)) in ours.lines.iter().zip(&oracle.lines).enumerate() {
                        assert_eq!(a, b, "row {row} ({context})");
                    }
                    assert_eq!(ours.line_numbers, oracle.line_numbers, "labels ({context})");
                    assert_eq!(
                        (
                            ours.active_first_row,
                            ours.active_screen_row,
                            ours.active_lines
                        ),
                        (
                            oracle.active_first_row,
                            oracle.active_screen_row,
                            oracle.active_lines
                        ),
                        "active anchors ({context})"
                    );
                }
            }
        }
    }

    /// Raw-view parity: the single-segment index assembled in full equals the
    /// legacy whole-document raw build.
    #[test]
    fn assembled_raw_index_matches_the_legacy_raw_build() {
        use crate::testdoc;

        let docs = [
            testdoc::many_blocks(12),
            testdoc::cjk_emoji(8),
            testdoc::tabs_and_wrap(8),
            String::new(),
        ];
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        for (doc_idx, src) in docs.iter().enumerate() {
            let rope = Rope::from_str(src);
            let layout = Layout::build(&rope, 30, 4);
            let len = rope.len_bytes();
            for cursor in [0, len / 2, len] {
                for selection in [None, Some((0, len.min(9)))] {
                    let oracle = build_raw_full(&rope, &layout, cursor, selection, &theme);
                    let mut view = build_raw_index(&rope, &layout);
                    let mut cache = ViewCache::default();
                    let cursor_line = rope
                        .byte_to_line(cursor)
                        .min(rope.len_lines().saturating_sub(1));
                    let total = view.total_rows();
                    let ours = assemble(
                        &mut view,
                        &mut cache,
                        &rope,
                        &layout,
                        cursor_line,
                        selection,
                        30,
                        &theme,
                        &hl,
                        0,
                        total,
                    );
                    let context = format!("doc {doc_idx}, cursor {cursor}, sel {selection:?}");
                    assert_eq!(ours.lines, oracle.lines, "rows ({context})");
                    assert_eq!(ours.numbers, oracle.line_numbers, "labels ({context})");
                }
            }
        }
    }

    /// Heights survive render-cache eviction: the row index stays computable
    /// without re-rendering, and totals never change across an eviction.
    #[test]
    fn heights_survive_render_cache_eviction() {
        let src = crate::testdoc::many_blocks(20);
        let rope = Rope::from_str(&src);
        let layout = Layout::build(&rope, 40, 4);
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let mut cache = ViewCache::default();

        let view = build_index(&mut cache, &rope, &layout, 0, 40, &theme, &hl, 0);
        let total = view.total_rows();
        let renders = cache.stats().block_renders;

        // Evict everything from the render cache; heights must remain.
        cache.rendered.clear();
        cache.rendered_bytes = 0;

        let rebuilt = build_index(&mut cache, &rope, &layout, 0, 40, &theme, &hl, 0);
        assert_eq!(rebuilt.total_rows(), total, "totals stable across eviction");
        assert_eq!(
            cache.stats().block_renders,
            renders,
            "rebuilding the index after eviction consults heights, not renders"
        );
    }
}
