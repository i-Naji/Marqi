//! Undo/redo history.
//!
//! Each [`Edit`] is a reversible replacement: at byte `start`, `removed` text
//! became `inserted` text. Consecutive same-kind edits are coalesced into one
//! undo step (a typing burst undoes at once) unless a cursor move, a newline, or
//! an idle pause breaks the run. The history stores edits but does not touch the
//! buffer — [`crate::app::App`] applies the (reverse) edit it hands back.

#[derive(Clone)]
pub struct Edit {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
    pub cursor_before: usize,
    pub cursor_after: usize,
}

/// Cap on retained undo steps. Beyond this the oldest steps are dropped, so a
/// long session cannot grow memory without bound.
const MAX_UNDO_STEPS: usize = 1_000;

pub struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Whether the top undo edit may still absorb the next one.
    open: bool,
    /// Steps dropped off the bottom of the undo stack by the cap. Counted so
    /// `depth` stays a stable marker even after old steps are gone.
    dropped: usize,
    /// `depth()` at the last save; `None` when the saved state is no longer
    /// reachable through undo/redo.
    saved_depth: Option<usize>,
}

impl History {
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            open: false,
            dropped: 0,
            saved_depth: Some(0),
        }
    }

    /// Stop coalescing: the next edit starts a fresh undo step. Called on cursor
    /// movement and other group boundaries.
    pub fn break_run(&mut self) {
        self.open = false;
    }

    /// Mark the current state as the on-disk state (called after a save). Also
    /// closes the run, so later typing cannot coalesce into a saved-state edit.
    pub fn mark_saved(&mut self) {
        self.open = false;
        self.saved_depth = Some(self.depth());
    }

    /// Whether undo/redo has returned the buffer to the last-saved state.
    pub fn at_saved_state(&self) -> bool {
        self.saved_depth == Some(self.depth())
    }

    fn depth(&self) -> usize {
        self.dropped + self.undo.len()
    }

    /// Record a new edit, coalescing it into the current run when possible.
    /// Any pending redo history is discarded.
    pub fn record(&mut self, edit: Edit) {
        if !self.redo.is_empty() {
            self.redo.clear();
            // If the saved state lived in the discarded redo branch, no undo
            // depth can reach it again — and a *different* future edit could
            // otherwise alias its depth and report a false "unmodified".
            if self.saved_depth.is_some_and(|d| d > self.depth()) {
                self.saved_depth = None;
            }
        }
        if self.open
            && let Some(top) = self.undo.last_mut()
            && merge(top, &edit)
        {
            // `merge` only succeeds for coalescable kinds, so the run stays open.
            return;
        }
        // A newline starts its own step and does not chain further typing.
        self.open = !edit.inserted.contains('\n');
        self.undo.push(edit);
        if self.undo.len() > MAX_UNDO_STEPS {
            let excess = self.undo.len() - MAX_UNDO_STEPS;
            self.undo.drain(..excess);
            self.dropped += excess;
        }
    }

    /// Update the cursor position replayed by redo for the edit just recorded.
    /// Used by operations (table row moves, renumbering) that place the cursor
    /// somewhere other than the end of the inserted text after the edit.
    pub fn set_last_cursor_after(&mut self, cursor: usize) {
        if let Some(top) = self.undo.last_mut() {
            top.cursor_after = cursor;
        }
    }

    /// Pop the most recent edit for undoing; moves it to the redo stack.
    pub fn undo(&mut self) -> Option<Edit> {
        let edit = self.undo.pop()?;
        self.open = false;
        self.redo.push(edit.clone());
        Some(edit)
    }

    /// Pop the most recent undone edit for redoing; moves it back to undo.
    pub fn redo(&mut self) -> Option<Edit> {
        let edit = self.redo.pop()?;
        self.open = false;
        self.undo.push(edit.clone());
        Some(edit)
    }
}

/// Try to fold `next` into `top` (same run). Returns true on success.
fn merge(top: &mut Edit, next: &Edit) -> bool {
    let both_insert = top.removed.is_empty() && next.removed.is_empty();
    let both_delete = top.inserted.is_empty() && next.inserted.is_empty();

    // Typing: contiguous inserts, and don't merge across a newline.
    if both_insert && !next.inserted.contains('\n') && next.start == top.start + top.inserted.len()
    {
        top.inserted.push_str(&next.inserted);
        top.cursor_after = next.cursor_after;
        return true;
    }
    // Backspace: deletions extending leftward.
    if both_delete && next.start + next.removed.len() == top.start {
        top.removed.insert_str(0, &next.removed);
        top.start = next.start;
        top.cursor_after = next.cursor_after;
        return true;
    }
    // Forward delete: deletions at the same position.
    if both_delete && next.start == top.start {
        top.removed.push_str(&next.removed);
        top.cursor_after = next.cursor_after;
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(start: usize, text: &str, before: usize) -> Edit {
        Edit {
            start,
            removed: String::new(),
            inserted: text.to_string(),
            cursor_before: before,
            cursor_after: before + text.len(),
        }
    }

    #[test]
    fn typing_coalesces_into_one_step() {
        let mut h = History::new();
        h.record(ins(0, "h", 0));
        h.record(ins(1, "i", 1));
        let e = h.undo().expect("one undo step");
        assert_eq!(e.inserted, "hi");
        assert!(h.undo().is_none(), "the burst undoes as a single step");
    }

    #[test]
    fn cursor_move_breaks_the_run() {
        let mut h = History::new();
        h.record(ins(0, "a", 0));
        h.break_run();
        h.record(ins(1, "b", 1));
        assert_eq!(h.undo().unwrap().inserted, "b");
        assert_eq!(h.undo().unwrap().inserted, "a");
    }

    #[test]
    fn redo_restores_after_undo() {
        let mut h = History::new();
        h.record(ins(0, "x", 0));
        let undone = h.undo().unwrap();
        assert_eq!(undone.inserted, "x");
        let redone = h.redo().unwrap();
        assert_eq!(redone.inserted, "x");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut h = History::new();
        h.record(ins(0, "a", 0));
        h.undo();
        h.record(ins(0, "b", 0));
        assert!(h.redo().is_none(), "a fresh edit discards the redo stack");
    }

    #[test]
    fn undo_back_to_the_saved_state_is_detected() {
        let mut h = History::new();
        assert!(h.at_saved_state(), "a fresh buffer is at its saved state");
        h.record(ins(0, "a", 0));
        h.mark_saved();
        h.record(ins(1, "b", 1)); // mark_saved closed the run: no coalescing
        assert!(!h.at_saved_state());
        h.undo();
        assert!(h.at_saved_state(), "undoing past-save edits restores it");
        h.redo();
        assert!(!h.at_saved_state());
    }

    #[test]
    fn saved_state_lost_to_a_cleared_redo_branch_never_aliases() {
        let mut h = History::new();
        h.record(ins(0, "a", 0));
        h.break_run();
        h.record(ins(1, "b", 1));
        h.mark_saved(); // saved at depth 2
        h.undo(); // depth 1
        h.record(ins(1, "c", 1)); // discards redo; depth 2 again, different text
        assert!(
            !h.at_saved_state(),
            "a different edit at the saved depth must not read as saved"
        );
    }

    #[test]
    fn undo_steps_are_capped() {
        let mut h = History::new();
        for i in 0..(MAX_UNDO_STEPS + 10) {
            h.record(ins(i, "x", i));
            h.break_run();
        }
        let mut steps = 0;
        while h.undo().is_some() {
            steps += 1;
        }
        assert_eq!(steps, MAX_UNDO_STEPS, "oldest steps fall off the bottom");
    }
}
