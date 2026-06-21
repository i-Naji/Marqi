//! Per-line incremental metadata: the tokenizer's fence state at the start of
//! every line, kept in lockstep with edits. Any window of the document can
//! then be styled identically to a whole-document scan without scanning from
//! the top — seed [`tokenizer::highlight_from`] with [`LineIndex::fence_at`].

use ropey::Rope;

use crate::markdown::tokenizer::{self, FenceState};

/// Packed [`FenceState`] per source line (2 bytes/line). Maintained by
/// [`LineIndex::apply_edit`]: edited lines are re-derived, then transitions
/// ripple forward only until they re-converge with the stored states (an edit
/// that flips a fence re-scans to the next former closer; ordinary edits stop
/// immediately).
pub struct LineIndex {
    fence: Vec<u16>,
}

impl LineIndex {
    pub fn build(rope: &Rope) -> Self {
        let total = rope.len_lines();
        let mut fence = Vec::with_capacity(total);
        let mut state = FenceState::default();
        let mut scratch = String::new();
        for line in 0..total {
            fence.push(state.pack());
            state = transition(rope, line, state, &mut scratch);
        }
        Self { fence }
    }

    /// Tokenizer state at the start of `line`.
    pub fn fence_at(&self, line: usize) -> FenceState {
        FenceState::unpack(self.fence.get(line).copied().unwrap_or(0))
    }

    /// Splice the edited lines and ripple transitions forward until the
    /// stored states re-converge. The `start_line`/`old_line_count`/
    /// `new_line_count` contract matches `Layout::update_after_edit`
    /// (computed on the pre-mutation rope).
    pub fn apply_edit(
        &mut self,
        rope: &Rope,
        start_line: usize,
        old_line_count: usize,
        new_line_count: usize,
    ) {
        let total = rope.len_lines();
        if total == 0 || self.fence.is_empty() {
            *self = Self::build(rope);
            return;
        }
        let start_line = start_line.min(total.saturating_sub(1));
        let cached = self.fence.len();
        let old_line_count = old_line_count
            .max(1)
            .min(cached.saturating_sub(start_line).max(1));
        let new_line_count = new_line_count
            .max(1)
            .min(total.saturating_sub(start_line).max(1));

        // The state *entering* the edit is unchanged — the edit begins on
        // `start_line`, so the stored state at its start still holds.
        let mut state = self.fence_at(start_line);
        self.fence.splice(
            start_line..(start_line + old_line_count).min(cached),
            std::iter::repeat_n(0u16, new_line_count),
        );

        let mut scratch = String::new();
        for line in start_line..start_line + new_line_count {
            self.fence[line] = state.pack();
            state = transition(rope, line, state, &mut scratch);
        }
        for line in start_line + new_line_count..total {
            if self.fence.get(line).copied() == Some(state.pack()) {
                return; // re-converged: everything below is already right
            }
            self.fence[line] = state.pack();
            state = transition(rope, line, state, &mut scratch);
        }
    }
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
        index.apply_edit(&rope, 1, 1, 1);
        let fresh = LineIndex::build(&rope);
        assert_eq!(index.fence, fresh.fence, "ripple matches a fresh build");
        assert!(!index.fence_at(2).is_inside(), "fence no longer opens");
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
                let new = text.bytes().filter(|b| *b == b'\n').count() + 1;
                rope.remove(rope.byte_to_char(start)..rope.byte_to_char(end));
                rope.insert(rope.byte_to_char(start), &text);
                index.apply_edit(&rope, start_line, old, new);

                let fresh = LineIndex::build(&rope);
                assert_eq!(
                    index.fence, fresh.fence,
                    "seed {seed:#x} step {step}: incremental == fresh"
                );
            }
        }
    }
}
