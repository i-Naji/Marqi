//! Per-line incremental metadata, kept in lockstep with edits:
//!
//! - the tokenizer's fence state at the start of every line, so any window of
//!   the document can be styled identically to a whole-document scan (seed
//!   [`tokenizer::highlight_from`] with [`LineIndex::fence_at`]);
//! - cheap per-line class flags (blank, footnote syntax, definition-shaped),
//!   which the block index uses for its windowed-reparse seam rules and
//!   full-reparse triggers.

use std::ops::Range;

use ropey::Rope;

use crate::markdown::tokenizer::{self, FenceState};

/// The line is entirely whitespace.
pub const BLANK: u8 = 1;
/// The line contains `[^` — footnote syntax (reference or definition). An
/// edit touching such a line can flip a *distant* definition between owned
/// block and gap (comrak drops unreferenced definitions from the AST), so the
/// block index always falls back to a full reparse around these.
pub const FOOTNOTE: u8 = 2;
/// The line looks like a link reference definition: first non-blank byte is
/// `[` and it contains `]:`. A superset of real definitions — false positives
/// only cost a spurious full reparse.
pub const DEF_COLON: u8 = 4;

/// What an edit did to the index, for the block index's trigger checks.
pub struct LineUpdate {
    /// OR of the replaced lines' pre-edit flags and the new lines' flags.
    pub touched_flags: u8,
    /// First line at/after the splice whose stored fence state re-converged
    /// (`== rope.len_lines()` when the ripple ran to the end of the document).
    /// Consumed by the two-sided window seam (lines whose fence state changed
    /// are dirty for parsing even when their text did not change).
    #[allow(dead_code)]
    pub resynced_at: usize,
}

/// Packed [`FenceState`] + class flags per source line (3 bytes/line).
pub struct LineIndex {
    fence: Vec<u16>,
    flags: Vec<u8>,
}

impl LineIndex {
    pub fn build(rope: &Rope) -> Self {
        let total = rope.len_lines();
        let mut fence = Vec::with_capacity(total);
        let mut flags = Vec::with_capacity(total);
        let mut state = FenceState::default();
        let mut scratch = String::new();
        for line in 0..total {
            fence.push(state.pack());
            flags.push(line_flags(rope, line));
            state = transition(rope, line, state, &mut scratch);
        }
        Self { fence, flags }
    }

    /// Tokenizer state at the start of `line`.
    pub fn fence_at(&self, line: usize) -> FenceState {
        FenceState::unpack(self.fence.get(line).copied().unwrap_or(0))
    }

    /// Class flags of `line`.
    pub fn flags_at(&self, line: usize) -> u8 {
        self.flags.get(line).copied().unwrap_or(0)
    }

    /// Whether any line in `range` carries any flag of `mask`.
    pub fn any_flags_in(&self, range: Range<usize>, mask: u8) -> bool {
        let end = range.end.min(self.flags.len());
        self.flags[range.start.min(end)..end]
            .iter()
            .any(|f| f & mask != 0)
    }

    /// Splice the edited lines and ripple fence transitions forward until the
    /// stored states re-converge. The `start_line`/`old_line_count`/
    /// `new_line_count` contract matches `Layout::update_after_edit`
    /// (computed on the pre-mutation rope).
    pub fn apply_edit(
        &mut self,
        rope: &Rope,
        start_line: usize,
        old_line_count: usize,
        new_line_count: usize,
    ) -> LineUpdate {
        let total = rope.len_lines();
        if total == 0 || self.fence.is_empty() {
            *self = Self::build(rope);
            return LineUpdate {
                touched_flags: u8::MAX,
                resynced_at: total,
            };
        }
        let start_line = start_line.min(total.saturating_sub(1));
        let cached = self.fence.len();
        let old_line_count = old_line_count
            .max(1)
            .min(cached.saturating_sub(start_line).max(1));
        let new_line_count = new_line_count
            .max(1)
            .min(total.saturating_sub(start_line).max(1));

        // Pre-edit flags of the replaced lines, before they vanish.
        let mut touched_flags = self.flags[start_line..(start_line + old_line_count).min(cached)]
            .iter()
            .fold(0, |acc, f| acc | f);
        // The state *entering* the edit is unchanged — the edit begins on
        // `start_line`, so the stored state at its start still holds. Read it
        // before the splice overwrites the slot.
        let mut state = self.fence_at(start_line);

        let splice = start_line..(start_line + old_line_count).min(cached);
        self.fence
            .splice(splice.clone(), std::iter::repeat_n(0u16, new_line_count));
        self.flags
            .splice(splice, std::iter::repeat_n(0u8, new_line_count));
        let mut scratch = String::new();
        for line in start_line..start_line + new_line_count {
            self.fence[line] = state.pack();
            let flags = line_flags(rope, line);
            self.flags[line] = flags;
            touched_flags |= flags;
            state = transition(rope, line, state, &mut scratch);
        }
        let mut resynced_at = total;
        for line in start_line + new_line_count..total {
            if self.fence.get(line).copied() == Some(state.pack()) {
                resynced_at = line;
                break; // re-converged: everything below is already right
            }
            self.fence[line] = state.pack();
            state = transition(rope, line, state, &mut scratch);
        }
        LineUpdate {
            touched_flags,
            resynced_at,
        }
    }
}

/// Class flags for one line, streamed over the rope chunks (no allocation).
fn line_flags(rope: &Rope, line: usize) -> u8 {
    let mut blank = true;
    let mut first_nonblank = 0u8;
    let mut prev = 0u8;
    let mut has_footnote = false;
    let mut has_defcolon = false;
    for chunk in rope.line(line).chunks() {
        for &b in chunk.as_bytes() {
            if blank && !b.is_ascii_whitespace() {
                blank = false;
                first_nonblank = b;
            }
            if prev == b'[' && b == b'^' {
                has_footnote = true;
            }
            if prev == b']' && b == b':' {
                has_defcolon = true;
            }
            prev = b;
        }
    }
    let mut flags = 0;
    if blank {
        flags |= BLANK;
    }
    if has_footnote {
        flags |= FOOTNOTE;
    }
    if has_defcolon && first_nonblank == b'[' {
        flags |= DEF_COLON;
    }
    flags
}

/// One line's fence transition. Only a line whose first non-blank byte is a
/// backtick or tilde can ever change the state, so everything else skips the
/// text copy entirely.
fn transition(rope: &Rope, line: usize, state: FenceState, scratch: &mut String) -> FenceState {
    let slice = rope.line(line);
    let mut first = None;
    'outer: for chunk in slice.chunks() {
        for &b in chunk.as_bytes() {
            if b != b' ' && b != b'\t' {
                first = Some(b);
                break 'outer;
            }
        }
    }
    if !matches!(first, Some(b'`') | Some(b'~')) {
        return state;
    }
    scratch.clear();
    for chunk in slice.chunks() {
        scratch.push_str(chunk);
    }
    tokenizer::fence_transition(scratch, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdoc::{self, XorShift};

    #[test]
    fn tracks_fences_and_ripples_on_edits() {
        let mut rope = Rope::from_str("a\n```\ncode\n```\nb\n");
        let mut index = LineIndex::build(&rope);
        assert!(
            !index.fence_at(1).is_inside(),
            "opening line starts outside"
        );
        assert!(index.fence_at(2).is_inside(), "interior is inside");
        assert!(index.fence_at(3).is_inside(), "closer line starts inside");
        assert!(!index.fence_at(4).is_inside(), "after the close");

        // Break the opening fence: everything below re-derives.
        let fence_start = rope.line_to_byte(1);
        rope.remove(rope.byte_to_char(fence_start)..rope.byte_to_char(fence_start + 1));
        let update = index.apply_edit(&rope, 1, 1, 1);
        let fresh = LineIndex::build(&rope);
        assert_eq!(index.fence, fresh.fence, "ripple matches a fresh build");
        assert!(!index.fence_at(2).is_inside(), "fence no longer opens");
        assert_eq!(
            update.resynced_at,
            rope.len_lines(),
            "breaking an opening fence ripples to the end"
        );
    }

    #[test]
    fn flags_classify_blank_footnote_and_definition_lines() {
        let rope = Rope::from_str(
            "plain\n\n[d]: /url\nuses a note[^n]\n[^n]: def body\n  [x]: indented def\nnot [a]: mid-line\n",
        );
        let index = LineIndex::build(&rope);
        assert_eq!(index.flags_at(0), 0);
        assert_eq!(index.flags_at(1), BLANK);
        assert_eq!(index.flags_at(2), DEF_COLON);
        assert_eq!(index.flags_at(3), FOOTNOTE);
        assert_eq!(index.flags_at(4), FOOTNOTE | DEF_COLON);
        assert_eq!(index.flags_at(5), DEF_COLON, "indented definitions count");
        assert_eq!(
            index.flags_at(6),
            0,
            "a ]: with a non-[ line start is not definition-shaped"
        );
        assert!(index.any_flags_in(0..rope.len_lines(), FOOTNOTE));
        assert!(!index.any_flags_in(0..2, FOOTNOTE | DEF_COLON));
    }

    #[test]
    fn touched_flags_report_pre_and_post_edit_lines() {
        // Deleting a definition line: its DEF_COLON flag must be reported even
        // though the line no longer exists after the edit.
        let mut rope = Rope::from_str("para\n\n[d]: /url\n\ntail\n");
        let mut index = LineIndex::build(&rope);
        let start = rope.line_to_byte(2);
        let end = rope.line_to_byte(3);
        rope.remove(rope.byte_to_char(start)..rope.byte_to_char(end));
        let update = index.apply_edit(&rope, 2, 2, 1);
        assert!(
            update.touched_flags & DEF_COLON != 0,
            "pre-edit flags survive into touched_flags"
        );

        // Typing footnote syntax: the new line's flag is reported.
        let mut rope = Rope::from_str("plain text\n");
        let mut index = LineIndex::build(&rope);
        rope.insert(rope.byte_to_char(5), "[^x]");
        let update = index.apply_edit(&rope, 0, 1, 1);
        assert!(update.touched_flags & FOOTNOTE != 0);
    }

    #[test]
    fn randomized_incremental_index_matches_fresh_builds() {
        for seed in [0xF00D, 0xFE2CE5] {
            let mut rng = XorShift::new(seed);
            let mut rope = Rope::from_str(&testdoc::random_doc(&mut rng, 80));
            let mut index = LineIndex::build(&rope);
            for step in 0..120 {
                let (start, end, text) = testdoc::random_edit(&mut rng, &rope);
                let start_line = rope.byte_to_line(start);
                let old = rope.byte_to_line(end) - start_line + 1;
                rope.remove(rope.byte_to_char(start)..rope.byte_to_char(end));
                rope.insert(rope.byte_to_char(start), &text);
                let inserted_end = (start + text.len()).min(rope.len_bytes());
                let new = rope.byte_to_line(inserted_end) - start_line + 1;
                index.apply_edit(&rope, start_line, old, new);

                let fresh = LineIndex::build(&rope);
                assert_eq!(
                    index.fence, fresh.fence,
                    "seed {seed:#x} step {step}: fence incremental == fresh"
                );
                assert_eq!(
                    index.flags, fresh.flags,
                    "seed {seed:#x} step {step}: flags incremental == fresh"
                );
            }
        }
    }
}
