//! The editing cursor.
//!
//! The cursor's source of truth is a **byte offset** into the rope. It also
//! carries a sticky `goal_col` so that a run of vertical moves keeps its
//! intended column across short lines (standard editor behaviour). Horizontal
//! motion steps whole grapheme clusters; vertical motion is by display row and
//! is driven by the [`Layout`].

use ropey::Rope;

use crate::layout::Layout;
use crate::text::{next_grapheme, prev_grapheme};

#[derive(Clone, Copy, Default)]
pub struct Cursor {
    /// Byte offset into the rope.
    pub byte: usize,
    /// Preferred display column for vertical movement.
    pub goal_col: u16,
}

impl Cursor {
    /// Move one grapheme cluster left.
    pub fn left(&mut self, rope: &Rope) {
        self.byte = prev_grapheme(rope, self.byte);
    }

    /// Move one grapheme cluster right.
    pub fn right(&mut self, rope: &Rope) {
        self.byte = next_grapheme(rope, self.byte);
    }

    /// Move up one display row, keeping the goal column.
    pub fn up(&mut self, layout: &Layout) {
        let (row, _) = layout.byte_to_pos(self.byte);
        if row > 0 {
            self.byte = layout.pos_to_byte(row - 1, self.goal_col);
        }
    }

    /// Move down one display row, keeping the goal column.
    pub fn down(&mut self, layout: &Layout) {
        let (row, _) = layout.byte_to_pos(self.byte);
        if row + 1 < layout.len() {
            self.byte = layout.pos_to_byte(row + 1, self.goal_col);
        }
    }

    /// Jump `rows` display rows up or down, keeping the goal column.
    pub fn page(&mut self, layout: &Layout, rows: usize, down: bool) {
        let (row, _) = layout.byte_to_pos(self.byte);
        let target = if down {
            (row + rows).min(layout.len().saturating_sub(1))
        } else {
            row.saturating_sub(rows)
        };
        self.byte = layout.pos_to_byte(target, self.goal_col);
    }

    /// Move to the start of the current logical line.
    pub fn home(&mut self, rope: &Rope) {
        let line = rope.byte_to_line(self.byte);
        self.byte = rope.line_to_byte(line);
    }

    /// Move to the end of the current logical line (before the newline).
    pub fn end(&mut self, rope: &Rope) {
        let line = rope.byte_to_line(self.byte);
        let slice = rope.line(line);
        let text = slice.to_string();
        let newline_len = text.len() - text.trim_end_matches(['\n', '\r']).len();
        self.byte = rope.line_to_byte(line) + slice.len_bytes() - newline_len;
    }

    /// Move to the start of the next word (Vim `w`).
    pub fn word_forward(&mut self, rope: &Rope) {
        let len = rope.len_bytes();
        let mut b = self.byte;
        let start = char_class(rope, b);
        if start != Class::Space {
            while b < len && char_class(rope, b) == start {
                b = next_grapheme(rope, b);
            }
        }
        while b < len && char_class(rope, b) == Class::Space {
            b = next_grapheme(rope, b);
        }
        self.byte = b;
    }

    /// Move to the start of the previous word (Vim `b`).
    pub fn word_back(&mut self, rope: &Rope) {
        if self.byte == 0 {
            return;
        }
        let mut b = prev_grapheme(rope, self.byte);
        while b > 0 && char_class(rope, b) == Class::Space {
            b = prev_grapheme(rope, b);
        }
        let cls = char_class(rope, b);
        while b > 0 && char_class(rope, prev_grapheme(rope, b)) == cls {
            b = prev_grapheme(rope, b);
        }
        self.byte = b;
    }

    /// Move to the start of the document.
    pub fn doc_start(&mut self) {
        self.byte = 0;
    }

    /// Move to the end of the document.
    pub fn doc_end(&mut self, rope: &Rope) {
        self.byte = rope.len_bytes();
    }

    /// Refresh the goal column from the current position. Call after any
    /// horizontal move or edit (but not between vertical moves).
    pub fn sync_goal(&mut self, layout: &Layout) {
        self.goal_col = layout.byte_to_pos(self.byte).1;
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

/// Classify the character at byte offset `byte` for word motions.
fn char_class(rope: &Rope, byte: usize) -> Class {
    if byte >= rope.len_bytes() {
        return Class::Space;
    }
    let c = rope.char(rope.byte_to_char(byte));
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}
