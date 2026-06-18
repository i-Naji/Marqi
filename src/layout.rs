//! Display layout: turn rope lines into cached [`DisplayLine`]s (visual rows)
//! and provide the bidirectional **byte ↔ (row, column)** mapping the cursor
//! and renderer rely on.
//!
//! A single logical line may produce several display lines via soft-wrap.
//! Columns are *display* columns: CJK and most emoji are width 2, combining
//! marks width 0, and tabs expand to the next tab stop. The newline itself is
//! never a cell — a hard-ended row's `byte_end` is the cursor's "end of line".
//!
//! Row geometry is kept as **exact per-line row counts** with chunked prefix
//! sums: row↔line lookups cost one chunk scan, and an edit recounts only the
//! lines it touched (no full-document metadata rebuild per keystroke). Byte
//! offsets come from an O(1) Arc-shared [`Rope`] snapshot instead of a
//! per-line table.

use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Lines per prefix-sum chunk: row lookups scan at most this many counts.
const CHUNK: usize = 1024;

/// One grapheme cluster as placed on screen.
///
/// Deliberately holds no text: the renderer slices the source line at
/// `byte..byte + len` and expands a tab to `width` spaces. Keeping the struct
/// at 12 heap-free bytes is what lets a full-document layout stay cheap.
pub struct Cell {
    /// Byte offset of this cluster's first byte, relative to its logical line.
    pub byte: u32,
    /// Byte length of the cluster (for grapheme-wise deletion/movement).
    pub len: u16,
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

/// Lifetime construction counters plus a snapshot of what is currently stored.
#[derive(Default, Clone, Copy)]
pub struct LayoutStats {
    /// Display rows constructed since the layout was created (cumulative).
    pub rows_built: usize,
    /// Cells constructed since the layout was created (cumulative).
    pub cells_built: usize,
    /// Display rows currently stored (snapshot).
    pub rows_live: usize,
}

/// The document laid out into display rows for a given wrap width.
pub struct Layout {
    lines: Vec<DisplayLine>,
    /// Exact display rows per source line (always ≥ 1).
    row_counts: Vec<u32>,
    /// `chunk_prefix[c]` = total rows of chunks `[0, c)`; the last entry is
    /// the document total.
    chunk_prefix: Vec<usize>,
    total_rows: usize,
    /// O(1) Arc-shared snapshot of the laid-out text, replaced on every
    /// build/update. Keeps byte↔position queries parameterless; staleness
    /// follows exactly the same contract as the row data itself.
    rope: Rope,
    wrap_width: usize,
    tab_width: usize,
    stats: LayoutStats,
}

impl Layout {
    /// Lay `rope` out, wrapping at `wrap_width` columns with `tab_width` tab stops.
    pub fn build(rope: &Rope, wrap_width: usize, tab_width: usize) -> Self {
        let mut layout = Self {
            lines: Vec::new(),
            row_counts: Vec::new(),
            chunk_prefix: Vec::new(),
            total_rows: 0,
            rope: rope.clone(),
            wrap_width: wrap_width.max(1),
            tab_width: tab_width.max(1),
            stats: LayoutStats::default(),
        };
        layout.rebuild_all(rope);
        layout
    }

    pub fn stats(&self) -> LayoutStats {
        LayoutStats {
            rows_live: self.lines.len(),
            ..self.stats
        }
    }

    /// Rebuild only the logical lines touched by an edit. `old_line_count`
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

        let row_start = self.first_row_of_line(start_line);
        let row_end = self.first_row_of_line(start_line + old_line_count);

        let line_delta = new_line_count as isize - old_line_count as isize;
        for row in &mut self.lines[row_end..] {
            row.line = row.line.saturating_add_signed(line_delta);
        }

        let mut replacement = Vec::new();
        let mut new_counts = Vec::with_capacity(new_line_count);
        for line in start_line..start_line + new_line_count {
            let built = build_line(rope, line, self.wrap_width, self.tab_width);
            new_counts.push(built.len() as u32);
            replacement.extend(built);
        }
        self.stats.rows_built += replacement.len();
        self.stats.cells_built += replacement.iter().map(|r| r.cells.len()).sum::<usize>();
        self.lines.splice(row_start..row_end, replacement);
        self.row_counts.splice(
            start_line..(start_line + old_line_count).min(cached_lines),
            new_counts,
        );
        self.rope = rope.clone();
        self.recompute_chunks();
    }

    /// Number of display rows (always ≥ 1).
    pub fn len(&self) -> usize {
        self.total_rows
    }

    /// Every display row. Test-only: the live pipeline goes through
    /// [`Layout::with_row`]/[`Layout::for_each_row`] so rows can later be
    /// materialized on demand instead of stored.
    #[cfg(test)]
    pub fn rows(&self) -> &[DisplayLine] {
        &self.lines
    }

    /// Visit one display row. Callback-style (rather than returning a
    /// reference) so a lazily-materializing layout can serve rows from an
    /// internal cache without leaking borrows.
    pub fn with_row<T>(&self, row: usize, f: impl FnOnce(&DisplayLine) -> T) -> T {
        let row = row.min(self.lines.len().saturating_sub(1));
        f(&self.lines[row])
    }

    /// Visit a contiguous range of display rows in order, passing each row's
    /// global index. The range is clamped to the document.
    pub fn for_each_row(
        &self,
        rows: std::ops::Range<usize>,
        mut f: impl FnMut(usize, &DisplayLine),
    ) {
        let end = rows.end.min(self.lines.len());
        for row in rows.start.min(end)..end {
            f(row, &self.lines[row]);
        }
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

    /// Absolute byte offset where a logical source line starts. Lines past the
    /// end of the document map to the last line's start.
    pub fn line_start(&self, line: usize) -> usize {
        let last = self.rope.len_lines().saturating_sub(1);
        self.rope.line_to_byte(line.min(last))
    }

    /// Map a rope byte offset to its `(row, display_col)` on screen.
    pub fn byte_to_pos(&self, byte: usize) -> (usize, u16) {
        let byte = byte.min(self.rope.len_bytes());
        let line = self.rope.byte_to_line(byte);
        let rel = byte.saturating_sub(self.line_start(line));
        let row_start = self.first_row_of_line(line);
        let row_end = self.first_row_of_line(line + 1);
        let mut r = row_start;

        while let Some(dl) = self.lines.get(r) {
            if rel >= dl.byte_start && rel < dl.byte_end {
                for cell in &dl.cells {
                    if rel < cell.byte_end() {
                        return (r, cell.col);
                    }
                }
                return (r, dl.width);
            }
            // End-of-line position only belongs to a hard-ended row; a
            // soft-wrap boundary is owned by the following row, which has the
            // same byte_start and is selected by the lookup above.
            if rel == dl.byte_end && !dl.soft_wrapped {
                return (r, dl.width);
            }
            if r + 1 < row_end
                && self
                    .lines
                    .get(r + 1)
                    .is_some_and(|next| rel >= next.byte_start)
            {
                r += 1;
                continue;
            }
            break;
        }
        let r = row_end
            .saturating_sub(1)
            .min(self.lines.len().saturating_sub(1));
        (r, self.lines.get(r).map_or(0, |d| d.width))
    }

    /// Map a `(row, target_col)` back to the nearest rope byte offset. `row` and
    /// `target_col` are clamped into range.
    pub fn pos_to_byte(&self, row: usize, target_col: u16) -> usize {
        let row = row.min(self.lines.len().saturating_sub(1));
        let dl = &self.lines[row];
        let line_start = self.line_start(dl.line);
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

    fn rebuild_all(&mut self, rope: &Rope) {
        self.rope = rope.clone();
        self.lines.clear();
        self.row_counts.clear();
        for line in 0..rope.len_lines() {
            let built = build_line(rope, line, self.wrap_width, self.tab_width);
            self.row_counts.push(built.len() as u32);
            self.lines.extend(built);
        }
        self.stats.rows_built += self.lines.len();
        self.stats.cells_built += self.lines.iter().map(|r| r.cells.len()).sum::<usize>();
        self.recompute_chunks();
    }

    /// Re-sum the chunk prefixes from `row_counts` — a single allocation-free
    /// pass of plain integer adds (a Fenwick tree could patch in O(log n), but
    /// only `first_row_of_line`/`len` read these sums, so the seam is small).
    fn recompute_chunks(&mut self) {
        self.chunk_prefix.clear();
        self.chunk_prefix.push(0);
        let mut total = 0usize;
        for (i, rows) in self.row_counts.iter().enumerate() {
            total += *rows as usize;
            if (i + 1) % CHUNK == 0 {
                self.chunk_prefix.push(total);
            }
        }
        if !self.row_counts.len().is_multiple_of(CHUNK) || self.row_counts.is_empty() {
            self.chunk_prefix.push(total);
        }
        self.total_rows = total;
    }
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
            off <= u32::MAX as usize && g.len() <= u16::MAX as usize,
            "logical line or grapheme exceeds Cell's compact field range"
        );
        row.cells.push(Cell {
            byte: off as u32,
            len: g.len() as u16,
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
    fn tabs_expand_to_stops() {
        let rope = Rope::from_str("\tx");
        let layout = Layout::build(&rope, 80, 4);
        // Tab fills columns 0..4, so x sits at col 4.
        assert_eq!(layout.byte_to_pos("\t".len()), (0, 4));
        let cell = &layout.rows()[0].cells[0];
        assert_eq!((cell.byte, cell.len, cell.width, cell.col), (0, 1, 4, 0));
    }

    #[test]
    fn soft_wraps_long_lines() {
        let rope = Rope::from_str("abcdef");
        let layout = Layout::build(&rope, 3, 4);
        assert_eq!(layout.len(), 2);
        assert!(layout.rows()[0].soft_wrapped);
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
    fn row_counts_and_chunk_sums_match_the_stored_rows() {
        let docs = [
            crate::testdoc::ascii(CHUNK + 50, 12), // spans a chunk boundary
            crate::testdoc::cjk_emoji(40),
            crate::testdoc::tabs_and_wrap(40),
            crate::testdoc::giant_block(4 * 1024),
            String::new(),
        ];
        for (i, doc) in docs.iter().enumerate() {
            for width in [1, 7, 80] {
                let rope = Rope::from_str(doc);
                let layout = Layout::build(&rope, width, 4);
                assert_eq!(layout.len(), layout.rows().len(), "doc {i} width {width}");
                let mut expected_first = 0usize;
                for line in 0..rope.len_lines() {
                    assert_eq!(
                        layout.first_row_of_line(line),
                        expected_first,
                        "doc {i} width {width} line {line}"
                    );
                    expected_first += layout.rows().iter().filter(|row| row.line == line).count();
                }
                assert_eq!(layout.first_row_of_line(rope.len_lines()), layout.len());
            }
        }
    }

    #[test]
    fn randomized_incremental_updates_match_fresh_builds() {
        use crate::testdoc::{self, XorShift};

        // Pre-mutation edit impact, mirroring `App::edit_impact`.
        fn impact(rope: &Rope, start: usize, end: usize, inserted: &str) -> (usize, usize, usize) {
            let start_line = rope.byte_to_line(start);
            let end_line = rope.byte_to_line(end);
            let old = end_line - start_line + 1;
            let new = inserted.bytes().filter(|b| *b == b'\n').count() + 1;
            (start_line, old, new)
        }

        for seed in [0xA11CE, 0xB0B5EED] {
            let mut rng = XorShift::new(seed);
            let mut rope = Rope::from_str(&testdoc::random_doc(&mut rng, 60));
            let width = 7 + rng.below(40);
            let mut layout = Layout::build(&rope, width, 4);

            for step in 0..50 {
                let (start, end, text) = testdoc::random_edit(&mut rng, &rope);
                let (start_line, old, new) = impact(&rope, start, end, &text);
                rope.remove(rope.byte_to_char(start)..rope.byte_to_char(end));
                rope.insert(rope.byte_to_char(start), &text);
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
            }
        }
    }
}
