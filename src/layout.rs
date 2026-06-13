//! Display layout: turn rope lines into cached [`DisplayLine`]s (visual rows)
//! and provide the bidirectional **byte ↔ (row, column)** mapping the cursor
//! and renderer rely on.
//!
//! A single logical line may produce several display lines via soft-wrap.
//! Columns are *display* columns: CJK and most emoji are width 2, combining
//! marks width 0, and tabs expand to the next tab stop. The newline itself is
//! never a cell — a hard-ended row's `byte_end` is the cursor's "end of line".

use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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
    line_first_rows: Vec<usize>,
    line_starts: Vec<usize>,
    wrap_width: usize,
    tab_width: usize,
    stats: LayoutStats,
}

impl Layout {
    /// Lay `rope` out, wrapping at `wrap_width` columns with `tab_width` tab stops.
    pub fn build(rope: &Rope, wrap_width: usize, tab_width: usize) -> Self {
        let wrap_width = wrap_width.max(1);
        let tab_width = tab_width.max(1);
        let mut layout = Self {
            lines: Vec::new(),
            line_first_rows: Vec::new(),
            line_starts: Vec::new(),
            wrap_width,
            tab_width,
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
        if total == 0 || self.line_first_rows.is_empty() {
            self.rebuild_all(rope);
            return;
        }

        let start_line = start_line.min(total.saturating_sub(1));
        let cached_lines = self.line_first_rows.len().saturating_sub(1);
        let old_line_count = old_line_count
            .max(1)
            .min(cached_lines.saturating_sub(start_line).max(1));
        let new_line_count = new_line_count
            .max(1)
            .min(total.saturating_sub(start_line).max(1));

        let row_start = self
            .line_first_rows
            .get(start_line)
            .copied()
            .unwrap_or(self.lines.len());
        let row_end = self
            .line_first_rows
            .get(start_line + old_line_count)
            .copied()
            .unwrap_or(self.lines.len());

        let line_delta = new_line_count as isize - old_line_count as isize;
        for row in &mut self.lines[row_end..] {
            row.line = row.line.saturating_add_signed(line_delta);
        }

        let mut replacement = Vec::new();
        for line in start_line..start_line + new_line_count {
            replacement.extend(build_line(rope, line, self.wrap_width, self.tab_width));
        }
        self.stats.rows_built += replacement.len();
        self.stats.cells_built += replacement.iter().map(|r| r.cells.len()).sum::<usize>();
        self.lines.splice(row_start..row_end, replacement);
        self.recompute_metadata(rope);
    }

    /// Number of display rows (always ≥ 1).
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn rows(&self) -> &[DisplayLine] {
        &self.lines
    }

    /// First display row of a logical source line. A line at or past the end
    /// of the document maps to `len()`.
    pub fn first_row_of_line(&self, line: usize) -> usize {
        self.line_first_rows
            .get(line)
            .copied()
            .unwrap_or(self.lines.len())
    }

    /// Absolute byte offset where a logical source line starts.
    pub fn line_start(&self, line: usize) -> usize {
        self.line_starts
            .get(line)
            .copied()
            .unwrap_or_else(|| self.line_starts.last().copied().unwrap_or(0))
    }

    /// Map a rope byte offset to its `(row, display_col)` on screen.
    pub fn byte_to_pos(&self, byte: usize) -> (usize, u16) {
        let line = self
            .line_starts
            .partition_point(|start| *start <= byte)
            .saturating_sub(1)
            .min(self.line_starts.len().saturating_sub(1));
        let rel = byte.saturating_sub(self.line_start(line));
        let row_start = self.line_first_rows.get(line).copied().unwrap_or(0);
        let row_end = self
            .line_first_rows
            .get(line + 1)
            .copied()
            .unwrap_or(self.lines.len());
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
            // same byte_start and is selected by the partition lookup above.
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
        self.lines.clear();
        for line in 0..rope.len_lines() {
            self.lines
                .extend(build_line(rope, line, self.wrap_width, self.tab_width));
        }
        self.stats.rows_built += self.lines.len();
        self.stats.cells_built += self.lines.iter().map(|r| r.cells.len()).sum::<usize>();
        self.recompute_metadata(rope);
    }

    fn recompute_metadata(&mut self, rope: &Rope) {
        let total = rope.len_lines();
        self.line_starts = (0..total).map(|line| rope.line_to_byte(line)).collect();
        // `lines.len()` doubles as the "unset" sentinel below — safe because a
        // real row index is always `< lines.len()`.
        self.line_first_rows = vec![self.lines.len(); total + 1];
        for (idx, row) in self.lines.iter().enumerate() {
            if row.line < total && self.line_first_rows[row.line] == self.lines.len() {
                self.line_first_rows[row.line] = idx;
            }
        }
        // Backfill row-less lines (in reverse) with the next line's first row,
        // making `line_first_rows` monotonically non-decreasing — an invariant
        // `update_after_edit` relies on for `row_start <= row_end` when it
        // splices the edited span.
        let mut next = self.lines.len();
        for line in (0..total).rev() {
            if self.line_first_rows[line] == self.lines.len() {
                self.line_first_rows[line] = next;
            } else {
                next = self.line_first_rows[line];
            }
        }
        self.line_first_rows[total] = self.lines.len();
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
}
