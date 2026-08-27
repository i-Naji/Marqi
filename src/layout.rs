//! Display layout: lazily materialize rope lines into [`DisplayLine`]s
//! (visual rows) and provide the bidirectional **byte ↔ (row, column)**
//! mapping the cursor and renderer rely on.
//!
//! A single logical line may produce several display lines via soft-wrap.
//! Columns are *display* columns: CJK and most emoji are width 2, combining
//! marks width 0, and tabs expand to the next tab stop. The newline itself is
//! never a cell — a hard-ended row's `byte_end` is the cursor's "end of line".
//!
//! The layout stores no cells for the document. It keeps **exact per-line row
//! counts** (an allocation-free counting pass; pure arithmetic for printable
//! ASCII) with chunked prefix sums for row↔line lookups, and materializes a
//! line's `DisplayLine`s only when a query or the renderer asks for them,
//! through a small bounded cache. An edit recounts only the lines it touched.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use ropey::{Rope, RopeSlice};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Lines per prefix-sum chunk: row lookups scan at most this many counts.
/// Small enough that per-lookup scans stay trivial even when the view layer
/// queries thousands of line boundaries per build; the prefix vector is still
/// only `lines / CHUNK` words.
const CHUNK: usize = 128;
/// Materialized-line cache cap. Eviction is clear-all: re-materializing a
/// line is cheap and bursty access (a viewport, a cursor) is local.
const ROW_CACHE_LINES: usize = 1024;

/// One grapheme cluster as placed on screen.
///
/// Deliberately holds no text: the renderer slices the source line at
/// `byte..byte + len` and expands a tab to `width` spaces. Keeping the struct
/// at 12 heap-free bytes is what keeps materialized lines cheap.
#[derive(Clone, Copy)]
pub struct Cell {
    /// Byte offset of this cluster's first byte, relative to its logical line.
    pub byte: u32,
    /// Byte length of the cluster (for grapheme-wise deletion/movement).
    pub len: u32,
    /// Display width in columns: 0, 1, or 2 (wider clusters are possible but rare).
    pub width: u16,
    /// Starting display column on this row.
    pub col: u16,
}

impl Cell {
    /// Line-relative byte offset of this cluster's first byte.
    pub fn byte(&self) -> usize {
        self.byte as usize
    }

    /// Line-relative byte offset one past this cluster's last byte.
    pub fn byte_end(&self) -> usize {
        self.byte as usize + self.len as usize
    }
}

/// One visual row. Owns the byte half-open range `[byte_start, byte_end)`.
#[derive(Clone)]
pub struct DisplayLine {
    pub cells: Vec<Cell>,
    /// 0-based logical source line that owns this visual row.
    pub line: usize,
    /// Relative byte offset from the start of `line`.
    pub byte_start: usize,
    /// One past the last content byte on this row. For a hard-ended row this is
    /// the cursor's end-of-line position; for a soft-wrapped row it belongs to
    /// the next row's `byte_start`.
    pub byte_end: usize,
    /// Total display width used by this row.
    pub width: u16,
    /// True when this row ends because of wrapping rather than a real newline.
    pub soft_wrapped: bool,
}

impl DisplayLine {
    fn new(line: usize, start: usize) -> Self {
        Self {
            cells: Vec::new(),
            line,
            byte_start: start,
            byte_end: start,
            width: 0,
            soft_wrapped: false,
        }
    }
}

/// Lifetime materialization counters plus a snapshot of what is currently
/// cached.
#[derive(Default, Clone, Copy)]
pub struct LayoutStats {
    /// Display rows materialized since the layout was created (cumulative).
    pub rows_built: usize,
    /// Cells materialized since the layout was created (cumulative).
    pub cells_built: usize,
    /// Display rows currently held by the materialization cache (snapshot).
    pub rows_live: usize,
}

#[derive(Default)]
struct RowCache {
    map: HashMap<usize, Rc<[DisplayLine]>>,
    rows_live: usize,
}

impl RowCache {
    fn clear(&mut self) {
        self.map.clear();
        self.rows_live = 0;
    }

    fn remove(&mut self, line: usize) {
        if let Some(rows) = self.map.remove(&line) {
            self.rows_live -= rows.len();
        }
    }
}

/// The document's display-row geometry for a given wrap width, with rows
/// materialized on demand.
pub struct Layout {
    /// Exact display rows per source line (always ≥ 1).
    row_counts: Vec<u32>,
    /// `chunk_prefix[c]` = total rows of chunks `[0, c)`; the last entry is
    /// the document total.
    chunk_prefix: Vec<usize>,
    total_rows: usize,
    /// O(1) Arc-shared snapshot of the laid-out text, replaced on every
    /// build/update. Keeps byte↔position queries parameterless; staleness
    /// follows exactly the same contract as the row counts themselves.
    rope: Rope,
    wrap_width: usize,
    tab_width: usize,
    /// Materialized lines, keyed by source line. Interior mutability so the
    /// long-standing `&Layout` query API keeps working at every call site.
    cache: RefCell<RowCache>,
    stats: std::cell::Cell<LayoutStats>,
}

impl Layout {
    /// Lay `rope` out, wrapping at `wrap_width` columns with `tab_width` tab
    /// stops. This only *counts* rows — no cells are materialized.
    pub fn build(rope: &Rope, wrap_width: usize, tab_width: usize) -> Self {
        let mut layout = Self {
            row_counts: Vec::new(),
            chunk_prefix: Vec::new(),
            total_rows: 0,
            rope: rope.clone(),
            wrap_width: wrap_width.max(1),
            tab_width: tab_width.max(1),
            cache: RefCell::new(RowCache::default()),
            stats: std::cell::Cell::new(LayoutStats::default()),
        };
        layout.rebuild_all(rope);
        layout
    }

    pub fn stats(&self) -> LayoutStats {
        LayoutStats {
            rows_live: self.cache.borrow().rows_live,
            ..self.stats.get()
        }
    }

    /// Recount only the logical lines touched by an edit. `old_line_count`
    /// describes how many cached source lines were replaced; `new_line_count`
    /// describes how many source lines now occupy that span.
    pub fn update_after_edit(
        &mut self,
        rope: &Rope,
        wrap_width: usize,
        tab_width: usize,
        start_line: usize,
        old_line_count: usize,
        new_line_count: usize,
    ) {
        let wrap_width = wrap_width.max(1);
        let tab_width = tab_width.max(1);
        if self.wrap_width != wrap_width || self.tab_width != tab_width {
            self.wrap_width = wrap_width;
            self.tab_width = tab_width;
            self.rebuild_all(rope);
            return;
        }

        let total = rope.len_lines();
        if total == 0 || self.row_counts.is_empty() {
            self.rebuild_all(rope);
            return;
        }

        let start_line = start_line.min(total.saturating_sub(1));
        let cached_lines = self.row_counts.len();
        let old_line_count = old_line_count
            .max(1)
            .min(cached_lines.saturating_sub(start_line).max(1));
        let new_line_count = new_line_count
            .max(1)
            .min(total.saturating_sub(start_line).max(1));

        let mut scratch = String::new();
        let mut new_counts = Vec::with_capacity(new_line_count);
        for line in start_line..start_line + new_line_count {
            new_counts.push(count_line_rows(
                rope,
                line,
                self.wrap_width,
                self.tab_width,
                &mut scratch,
            ));
        }
        self.row_counts.splice(
            start_line..(start_line + old_line_count).min(cached_lines),
            new_counts,
        );

        // Materialized lines after a line-count change would keep stale
        // `line` fields; drop them and re-materialize on demand. A same-count
        // edit only invalidates the lines whose text changed.
        let mut cache = self.cache.borrow_mut();
        if old_line_count == new_line_count {
            for line in start_line..start_line + new_line_count {
                cache.remove(line);
            }
        } else {
            let mut removed = 0;
            cache.map.retain(|&line, rows| {
                let keep = line < start_line;
                if !keep {
                    removed += rows.len();
                }
                keep
            });
            cache.rows_live -= removed;
        }
        drop(cache);

        self.rope = rope.clone();
        self.recompute_chunks();
    }

    /// Number of display rows (always ≥ 1).
    pub fn len(&self) -> usize {
        self.total_rows
    }

    /// The materialized display rows of one source line (built on demand).
    pub fn line_rows(&self, line: usize) -> Rc<[DisplayLine]> {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        if let Some(rows) = self.cache.borrow().map.get(&line) {
            return rows.clone();
        }
        let rows: Rc<[DisplayLine]> =
            build_line(&self.rope, line, self.wrap_width, self.tab_width).into();
        debug_assert_eq!(
            rows.len(),
            self.row_counts.get(line).copied().unwrap_or(1) as usize,
            "materialized rows must match the counted rows for line {line}"
        );
        let mut stats = self.stats.get();
        stats.rows_built += rows.len();
        stats.cells_built += rows.iter().map(|r| r.cells.len()).sum::<usize>();
        self.stats.set(stats);
        let mut cache = self.cache.borrow_mut();
        if cache.map.len() >= ROW_CACHE_LINES {
            cache.clear();
        }
        cache.rows_live += rows.len();
        cache.map.insert(line, rows.clone());
        rows
    }

    /// Visit one display row. Callback-style (rather than returning a
    /// reference) so rows can be served from the materialization cache
    /// without leaking borrows.
    pub fn with_row<T>(&self, row: usize, f: impl FnOnce(&DisplayLine) -> T) -> T {
        let row = row.min(self.total_rows.saturating_sub(1));
        let (line, first) = self.line_at_row(row);
        let rows = self.line_rows(line);
        let idx = (row - first).min(rows.len().saturating_sub(1));
        f(&rows[idx])
    }

    /// Visit a contiguous range of display rows in order, passing each row's
    /// global index. The range is clamped to the document; each source line
    /// materializes once.
    pub fn for_each_row(&self, range: Range<usize>, mut f: impl FnMut(usize, &DisplayLine)) {
        let end = range.end.min(self.total_rows);
        let mut row = range.start.min(end);
        if row >= end {
            return;
        }
        let (mut line, mut first) = self.line_at_row(row);
        while row < end && line < self.row_counts.len() {
            let rows = self.line_rows(line);
            let upto = (first + rows.len()).min(end);
            for r in row..upto {
                f(r, &rows[r - first]);
            }
            first += rows.len();
            row = first;
            line += 1;
        }
    }

    /// Every display row, cloned out of the lazy cache. Test-only convenience
    /// for the legacy full-build oracles.
    #[cfg(test)]
    pub fn collect_rows(&self, range: Range<usize>) -> Vec<DisplayLine> {
        let mut out = Vec::new();
        self.for_each_row(range, |_, dl| out.push(dl.clone()));
        out
    }

    /// First display row of a logical source line. A line at or past the end
    /// of the document maps to `len()`.
    pub fn first_row_of_line(&self, line: usize) -> usize {
        if line >= self.row_counts.len() {
            return self.total_rows;
        }
        let chunk = line / CHUNK;
        let mut row = self.chunk_prefix[chunk];
        for rows in &self.row_counts[chunk * CHUNK..line] {
            row += *rows as usize;
        }
        row
    }

    /// `(source line, that line's first global row)` for a display row.
    fn line_at_row(&self, row: usize) -> (usize, usize) {
        if self.row_counts.is_empty() {
            return (0, 0);
        }
        let row = row.min(self.total_rows.saturating_sub(1));
        let chunk = self
            .chunk_prefix
            .partition_point(|start| *start <= row)
            .saturating_sub(1)
            .min(self.chunk_prefix.len().saturating_sub(2));
        let mut line = chunk * CHUNK;
        let mut first = self.chunk_prefix[chunk];
        loop {
            let rows = self.row_counts[line] as usize;
            if row < first + rows || line + 1 >= self.row_counts.len() {
                return (line, first);
            }
            first += rows;
            line += 1;
        }
    }

    /// Absolute byte offset where a logical source line starts. Lines past the
    /// end of the document map to the last line's start.
    pub fn line_start(&self, line: usize) -> usize {
        let last = self.rope.len_lines().saturating_sub(1);
        self.rope.line_to_byte(line.min(last))
    }

    /// Map a rope byte offset to its `(row, display_col)` on screen. Only the
    /// byte's own line is materialized.
    pub fn byte_to_pos(&self, byte: usize) -> (usize, u16) {
        let byte = byte.min(self.rope.len_bytes());
        let line = self.rope.byte_to_line(byte);
        let rel = byte.saturating_sub(self.line_start(line));
        let first = self.first_row_of_line(line);
        let rows = self.line_rows(line);

        for (i, dl) in rows.iter().enumerate() {
            if rel >= dl.byte_start && rel < dl.byte_end {
                for cell in &dl.cells {
                    if rel < cell.byte_end() {
                        return (first + i, cell.col);
                    }
                }
                return (first + i, dl.width);
            }
            // End-of-line position only belongs to a hard-ended row; a
            // soft-wrap boundary is owned by the following row.
            if rel == dl.byte_end && !dl.soft_wrapped {
                return (first + i, dl.width);
            }
            if i + 1 < rows.len() && rel >= rows[i + 1].byte_start {
                continue;
            }
            break;
        }
        let i = rows.len() - 1;
        (first + i, rows[i].width)
    }

    /// Map a `(row, target_col)` back to the nearest rope byte offset. `row` and
    /// `target_col` are clamped into range.
    pub fn pos_to_byte(&self, row: usize, target_col: u16) -> usize {
        let row = row.min(self.total_rows.saturating_sub(1));
        let (line, first) = self.line_at_row(row);
        let rows = self.line_rows(line);
        let dl = &rows[(row - first).min(rows.len().saturating_sub(1))];
        let line_start = self.line_start(line);
        if dl.cells.is_empty() {
            return line_start + dl.byte_start;
        }
        if target_col >= dl.width {
            // Past the content: end of line for a hard row, last cluster for a
            // soft-wrapped row (so we stay on this visual row).
            return line_start
                + if dl.soft_wrapped {
                    dl.cells.last().map_or(dl.byte_end, |c| c.byte())
                } else {
                    dl.byte_end
                };
        }
        for cell in &dl.cells {
            if target_col < cell.col + cell.width {
                return line_start + cell.byte();
            }
        }
        line_start + dl.cells.last().map_or(dl.byte_start, |c| c.byte())
    }

    /// Recount every line (allocation-free aside from one reusable scratch).
    fn rebuild_all(&mut self, rope: &Rope) {
        self.rope = rope.clone();
        self.row_counts.clear();
        let total = rope.len_lines();
        self.row_counts.reserve(total);
        let mut scratch = String::new();
        for line in 0..total {
            self.row_counts.push(count_line_rows(
                rope,
                line,
                self.wrap_width,
                self.tab_width,
                &mut scratch,
            ));
        }
        self.cache.borrow_mut().clear();
        self.recompute_chunks();
    }

    /// Re-sum the chunk prefixes from `row_counts` — a single allocation-free
    /// pass of plain integer adds (a Fenwick tree could patch in O(log n), but
    /// only `first_row_of_line`/`line_at_row`/`len` read these sums, so the
    /// seam is small).
    fn recompute_chunks(&mut self) {
        self.chunk_prefix.clear();
        self.chunk_prefix.push(0);
        let mut total = 0usize;
        for (i, rows) in self.row_counts.iter().enumerate() {
            total += *rows as usize;
            if (i + 1).is_multiple_of(CHUNK) {
                self.chunk_prefix.push(total);
            }
        }
        if !self.row_counts.len().is_multiple_of(CHUNK) || self.row_counts.is_empty() {
            self.chunk_prefix.push(total);
        }
        self.total_rows = total;
    }
}

/// Exact display-row count for one source line. Printable-ASCII lines (the
/// common case) are pure arithmetic: every byte is one width-1 cell, so wrap
/// boundaries fall exactly at multiples of the width. Anything else (tabs,
/// control bytes, non-ASCII) runs the same walk as [`build_line`] against a
/// reusable scratch buffer.
fn count_line_rows(
    rope: &Rope,
    line: usize,
    wrap_width: usize,
    tab_width: usize,
    scratch: &mut String,
) -> u32 {
    let slice = rope.line(line);
    if let Some(rows) = ascii_fast_rows(&slice, wrap_width.max(1)) {
        return rows;
    }
    scratch.clear();
    for chunk in slice.chunks() {
        scratch.push_str(chunk);
    }
    let content = scratch.trim_end_matches(['\n', '\r']);
    count_rows_by_walk(content, wrap_width, tab_width)
}

/// Row count for a line of printable ASCII (plus a trailing `\n`/`\r` run),
/// or `None` when the line needs the full grapheme walk.
fn ascii_fast_rows(slice: &RopeSlice, wrap: usize) -> Option<u32> {
    let mut content_len = 0usize;
    let mut in_terminator = false;
    for chunk in slice.chunks() {
        for &b in chunk.as_bytes() {
            match b {
                0x20..=0x7E if !in_terminator => content_len += 1,
                b'\n' | b'\r' => in_terminator = true,
                _ => return None,
            }
        }
    }
    Some(content_len.div_ceil(wrap).max(1) as u32)
}

/// The wrap walk of [`build_line`], counting rows instead of building cells.
/// Must stay in lockstep with it — `line_rows` debug-asserts the equivalence
/// and the randomized layout tests cross-check it on every document shape.
fn count_rows_by_walk(content: &str, wrap_width: usize, tab_width: usize) -> u32 {
    let wrap = wrap_width.max(1) as u16;
    let tab = tab_width.max(1);
    let mut rows: u32 = 1;
    let mut col: u16 = 0;
    let mut row_has_cells = false;
    for g in content.graphemes(true) {
        if col + grapheme_width(g, col, tab) > wrap && row_has_cells {
            rows += 1;
            col = 0;
        }
        // The cluster is always placed on the (possibly fresh) row.
        col += grapheme_width(g, col, tab);
        row_has_cells = true;
    }
    rows
}

fn build_line(rope: &Rope, line: usize, wrap_width: usize, tab_width: usize) -> Vec<DisplayLine> {
    let wrap = wrap_width.max(1) as u16;
    let tab = tab_width.max(1);
    let slice = rope.line(line);
    let text = slice.to_string();
    let content = text.trim_end_matches(['\n', '\r']);
    let mut lines = Vec::new();

    let mut row = DisplayLine::new(line, 0);
    let mut col: u16 = 0;
    for (off, g) in content.grapheme_indices(true) {
        // Soft-wrap before placing a cluster that would overflow a non-empty
        // row (a single oversized cluster still gets placed).
        if col + grapheme_width(g, col, tab) > wrap && !row.cells.is_empty() {
            row.byte_end = off;
            row.soft_wrapped = true;
            row.width = col;
            lines.push(std::mem::replace(&mut row, DisplayLine::new(line, off)));
            col = 0;
        }
        let width = grapheme_width(g, col, tab);
        debug_assert!(
            off <= u32::MAX as usize,
            "logical line exceeds Cell's compact field range"
        );
        row.cells.push(Cell {
            byte: off as u32,
            len: g.len() as u32,
            width,
            col,
        });
        col += width;
    }

    row.byte_end = content.len();
    row.width = col;
    lines.push(row);
    lines
}

/// Display width of a single grapheme cluster at column `col`.
fn grapheme_width(g: &str, col: u16, tab: usize) -> u16 {
    if g == "\t" {
        (tab - (col as usize % tab)) as u16
    } else {
        UnicodeWidthStr::width(g) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_cjk_and_emoji_columns() {
        // "a中b": a@col0(w1), 中@col1(w2), b@col3(w1).
        let rope = Rope::from_str("a中b");
        let layout = Layout::build(&rope, 80, 4);
        assert_eq!(layout.len(), 1);

        assert_eq!(layout.byte_to_pos(0), (0, 0)); // a
        assert_eq!(layout.byte_to_pos("a".len()), (0, 1)); // 中
        assert_eq!(layout.byte_to_pos("a中".len()), (0, 3)); // b
        assert_eq!(layout.byte_to_pos("a中b".len()), (0, 4)); // end of line

        // Clicking inside the wide char snaps to its start byte.
        assert_eq!(layout.pos_to_byte(0, 2), "a".len());
        assert_eq!(layout.pos_to_byte(0, 1), "a".len());
    }

    #[test]
    fn long_grapheme_clusters_keep_their_full_length() {
        let text = format!("a{}", "\u{20dd}".repeat(40_000));
        let rope = Rope::from_str(&text);
        let layout = Layout::build(&rope, 80, 4);
        assert_eq!(layout.pos_to_byte(0, 5), text.len());
    }

    #[test]
    fn tabs_expand_to_stops() {
        let rope = Rope::from_str("\tx");
        let layout = Layout::build(&rope, 80, 4);
        // Tab fills columns 0..4, so x sits at col 4.
        assert_eq!(layout.byte_to_pos("\t".len()), (0, 4));
        let rows = layout.line_rows(0);
        let cell = &rows[0].cells[0];
        assert_eq!((cell.byte, cell.len, cell.width, cell.col), (0, 1, 4, 0));
    }

    #[test]
    fn soft_wraps_long_lines() {
        let rope = Rope::from_str("abcdef");
        let layout = Layout::build(&rope, 3, 4);
        assert_eq!(layout.len(), 2);
        assert!(layout.line_rows(0)[0].soft_wrapped);
        // 'd' starts the second visual row.
        assert_eq!(layout.byte_to_pos(3), (1, 0));
        // Round-trips back.
        assert_eq!(layout.pos_to_byte(1, 0), 3);
    }

    #[test]
    fn blank_and_trailing_lines_are_addressable() {
        // "a\n\nb\n": content line, blank line, content line, trailing empty line.
        let rope = Rope::from_str("a\n\nb\n");
        let layout = Layout::build(&rope, 80, 4);
        assert_eq!(layout.len(), 4);
        assert_eq!(layout.byte_to_pos(2), (1, 0)); // the blank line
        assert_eq!(layout.byte_to_pos(5), (3, 0)); // trailing empty line at EOF
    }

    #[test]
    fn incremental_line_update_matches_fresh_layout() {
        let mut rope = Rope::from_str("abc\ndef\n");
        let mut layout = Layout::build(&rope, 80, 4);
        rope.insert(1, "X");
        layout.update_after_edit(&rope, 80, 4, 0, 1, 1);
        let fresh = Layout::build(&rope, 80, 4);

        assert_eq!(layout.len(), fresh.len());
        assert_eq!(layout.byte_to_pos(2), fresh.byte_to_pos(2));
        assert_eq!(
            layout.byte_to_pos(rope.len_bytes()),
            fresh.byte_to_pos(rope.len_bytes())
        );
        assert_eq!(layout.pos_to_byte(1, 1), fresh.pos_to_byte(1, 1));
    }

    #[test]
    fn counting_matches_materialization_across_shapes() {
        let docs = [
            crate::testdoc::ascii(CHUNK + 50, 12), // spans a chunk boundary
            crate::testdoc::cjk_emoji(40),
            crate::testdoc::tabs_and_wrap(40),
            crate::testdoc::giant_block(4 * 1024),
            "interior\rreturn and trailing run\r\r\n".to_string(),
            String::new(),
        ];
        for (i, doc) in docs.iter().enumerate() {
            for width in [1, 7, 80] {
                let rope = Rope::from_str(doc);
                let layout = Layout::build(&rope, width, 4);
                let mut expected_first = 0usize;
                for line in 0..rope.len_lines() {
                    assert_eq!(
                        layout.first_row_of_line(line),
                        expected_first,
                        "doc {i} width {width} line {line}"
                    );
                    // The counted rows must equal the materialized rows.
                    expected_first += layout.line_rows(line).len();
                }
                assert_eq!(layout.first_row_of_line(rope.len_lines()), layout.len());
                assert_eq!(layout.len(), expected_first, "doc {i} width {width}");
            }
        }
    }

    #[test]
    fn build_materializes_nothing_until_queried() {
        let rope = Rope::from_str(&crate::testdoc::ascii(5_000, 60));
        let layout = Layout::build(&rope, 40, 4);
        assert_eq!(
            layout.stats().cells_built,
            0,
            "building the layout is a counting pass only"
        );
        let _ = layout.byte_to_pos(rope.len_bytes() / 2);
        let stats = layout.stats();
        assert!(stats.cells_built > 0, "queries materialize on demand");
        assert!(
            stats.rows_live <= 2,
            "one query materializes one line, got {} rows",
            stats.rows_live
        );
    }

    #[test]
    fn row_cache_stays_bounded_and_correct_after_eviction() {
        let rope = Rope::from_str(&crate::testdoc::ascii(3 * ROW_CACHE_LINES, 10));
        let layout = Layout::build(&rope, 40, 4);
        for line in 0..rope.len_lines() {
            let _ = layout.line_rows(line);
        }
        assert!(
            layout.stats().rows_live <= ROW_CACHE_LINES + 1,
            "cache stays bounded, got {}",
            layout.stats().rows_live
        );
        // Eviction never changes answers.
        assert_eq!(layout.byte_to_pos(0), (0, 0));
        let byte = rope.line_to_byte(7) + 3;
        assert_eq!(layout.byte_to_pos(byte), (7, 3));
        assert_eq!(layout.pos_to_byte(7, 3), byte);
    }

    #[test]
    fn randomized_incremental_updates_match_fresh_builds() {
        use crate::testdoc::{self, XorShift};

        for seed in [0xA11CE, 0xB0B5EED] {
            let mut rng = XorShift::new(seed);
            let mut rope = Rope::from_str(&testdoc::random_doc(&mut rng, 60));
            let width = 7 + rng.below(40);
            let mut layout = Layout::build(&rope, width, 4);

            for step in 0..50 {
                let (start, end, text) = testdoc::random_edit(&mut rng, &rope);
                // Pre-mutation impact + post-mutation new-line count, exactly
                // like `App::mark_edited`.
                let start_line = rope.byte_to_line(start);
                let old = rope.byte_to_line(end) - start_line + 1;
                rope.remove(rope.byte_to_char(start)..rope.byte_to_char(end));
                rope.insert(rope.byte_to_char(start), &text);
                let inserted_end = (start + text.len()).min(rope.len_bytes());
                let new = rope.byte_to_line(inserted_end) - start_line + 1;
                layout.update_after_edit(&rope, width, 4, start_line, old, new);

                let fresh = Layout::build(&rope, width, 4);
                let context = format!("seed {seed:#x} step {step} width {width}");
                assert_eq!(layout.len(), fresh.len(), "len ({context})");
                for _ in 0..16 {
                    let byte = rng.below(rope.len_bytes() + 1);
                    assert_eq!(
                        layout.byte_to_pos(byte),
                        fresh.byte_to_pos(byte),
                        "byte_to_pos({byte}) ({context})"
                    );
                }
                for _ in 0..16 {
                    let row = rng.below(fresh.len());
                    let col = rng.below(width + 2) as u16;
                    assert_eq!(
                        layout.pos_to_byte(row, col),
                        fresh.pos_to_byte(row, col),
                        "pos_to_byte({row},{col}) ({context})"
                    );
                }
                for _ in 0..8 {
                    let line = rng.below(rope.len_lines() + 1);
                    assert_eq!(
                        layout.first_row_of_line(line),
                        fresh.first_row_of_line(line),
                        "first_row_of_line({line}) ({context})"
                    );
                }
                // Full row equality for a sampled line: cells, byte ranges,
                // wrap flags — materialization agrees with a fresh build.
                let line = rng.below(rope.len_lines());
                let (ours, theirs) = (layout.line_rows(line), fresh.line_rows(line));
                assert_eq!(ours.len(), theirs.len(), "line {line} rows ({context})");
                for (a, b) in ours.iter().zip(theirs.iter()) {
                    assert_eq!(
                        (a.line, a.byte_start, a.byte_end, a.width, a.soft_wrapped),
                        (b.line, b.byte_start, b.byte_end, b.width, b.soft_wrapped),
                        "row shape, line {line} ({context})"
                    );
                    assert_eq!(a.cells.len(), b.cells.len(), "cells, line {line}");
                    for (ca, cb) in a.cells.iter().zip(&b.cells) {
                        assert_eq!(
                            (ca.byte, ca.len, ca.width, ca.col),
                            (cb.byte, cb.len, cb.width, cb.col),
                            "cell, line {line} ({context})"
                        );
                    }
                }
            }
        }
    }
}
