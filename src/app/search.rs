//! Find and replace.
//!
//! `open_find` opens an incremental find prompt: typing jumps to the first
//! match at or after where the search started, Enter/↓ and ↑ step through
//! matches (wrapping), and Tab switches to a replace prompt where Enter
//! replaces the current match and `^A` replaces every match as one undo step.
//! The current match is shown as a selection, so it is visible in any view.
//!
//! Matching is plain-text with smart case: an all-lowercase query matches
//! case-insensitively (ASCII), any uppercase makes it exact.

use super::App;
use super::prompt::Prompt;

impl App {
    /// Open the find prompt, seeded with the current selection (if it is a
    /// reasonable query) or the previous search.
    pub(super) fn open_find(&mut self) {
        let seed = self
            .selection_range()
            .map(|(s, e)| self.buffer.slice(s, e))
            .filter(|t| !t.is_empty() && !t.contains('\n') && t.len() <= 200)
            .unwrap_or_else(|| self.last_search.clone());
        self.find_origin = self
            .selection_range()
            .map(|(s, _)| s)
            .unwrap_or(self.cursor.byte);
        self.prompt = Some(Prompt::Find {
            cursor: seed.len(),
            input: seed,
        });
    }

    pub(super) fn find_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        mut input: String,
        mut cursor: usize,
    ) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => {
                self.last_search = input;
                return;
            }
            KeyCode::Enter | KeyCode::Down => {
                self.last_search = input.clone();
                self.find_step(&input, true, true);
            }
            KeyCode::Up => {
                self.last_search = input.clone();
                self.find_step(&input, false, true);
            }
            KeyCode::Tab if !input.is_empty() => {
                self.last_search = input.clone();
                self.prompt = Some(Prompt::Replace {
                    query: input,
                    input: String::new(),
                    cursor: 0,
                });
                return;
            }
            _ => {
                if super::prompt::edit_line(key, &mut input, &mut cursor) {
                    // Incremental: re-run from where the search started, so
                    // refining the query does not walk away down the file.
                    self.find_from_origin(&input);
                }
            }
        }
        self.prompt = Some(Prompt::Find { input, cursor });
    }

    pub(super) fn replace_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        query: String,
        mut input: String,
        mut cursor: usize,
    ) {
        use crossterm::event::KeyCode;
        let ctrl = super::ctrl_like(key.modifiers);
        match key.code {
            KeyCode::Esc => {
                self.last_search = query;
                return;
            }
            KeyCode::Tab => {
                let cursor = query.len();
                self.prompt = Some(Prompt::Find {
                    input: query,
                    cursor,
                });
                return;
            }
            KeyCode::Char('a') | KeyCode::Char('A') if ctrl => {
                let n = self.replace_all(&query, &input);
                self.status = Some(match n {
                    0 => "No matches".to_string(),
                    1 => "Replaced 1 occurrence".to_string(),
                    n => format!("Replaced {n} occurrences"),
                });
                self.last_search = query;
                return;
            }
            KeyCode::Enter => self.replace_current(&query, &input),
            _ => {
                super::prompt::edit_line(key, &mut input, &mut cursor);
            }
        }
        self.prompt = Some(Prompt::Replace {
            query,
            input,
            cursor,
        });
    }

    /// Jump to the next/previous match relative to the current one (Vim
    /// `n`/`N`), reusing the last query. Places the cursor at the match start
    /// without selecting, so `x`/`d` keep their single-target meaning.
    pub(super) fn find_next(&mut self, forward: bool) {
        if self.last_search.is_empty() {
            self.status = Some("No previous search".to_string());
            return;
        }
        let query = self.last_search.clone();
        self.find_step(&query, forward, false);
    }

    /// Every match start, in order. Smart case: an all-lowercase query
    /// matches ASCII case-insensitively. Byte-wise comparison is boundary-safe
    /// because non-ASCII bytes only match exactly.
    pub(super) fn search_matches(&self, query: &str) -> Vec<usize> {
        if query.is_empty() {
            return Vec::new();
        }
        let source = self.buffer.rope().to_string();
        let haystack = source.as_bytes();
        let needle = query.as_bytes();
        let exact = query.chars().any(|c| c.is_uppercase());
        let mut out = Vec::new();
        let mut i = 0;
        while i + needle.len() <= haystack.len() {
            let window = &haystack[i..i + needle.len()];
            let hit = if exact {
                window == needle
            } else {
                window.eq_ignore_ascii_case(needle)
            };
            if hit {
                out.push(i);
                i += needle.len();
            } else {
                i += 1;
            }
        }
        out
    }

    /// Jump to the first match at or after where the search started (used
    /// while typing the query). Wraps to the first match in the document.
    fn find_from_origin(&mut self, query: &str) {
        let matches = self.search_matches(query);
        let Some(&start) = matches
            .iter()
            .find(|&&s| s >= self.find_origin)
            .or_else(|| matches.first())
        else {
            self.selection_anchor = None;
            return;
        };
        self.jump_to_match(start, query.len(), true);
    }

    /// Step to the next/previous match from the current position, wrapping.
    fn find_step(&mut self, query: &str, forward: bool, select: bool) {
        let matches = self.search_matches(query);
        if matches.is_empty() {
            self.status = Some("No matches".to_string());
            return;
        }
        let from = self
            .selection_range()
            .map(|(s, _)| s)
            .unwrap_or(self.cursor.byte);
        // When sitting on a match, step off it; otherwise a match exactly at
        // the cursor is the natural first hit.
        let on_match = matches.contains(&from);
        let next = if forward {
            matches
                .iter()
                .find(|&&s| if on_match { s > from } else { s >= from })
                .or_else(|| matches.first())
        } else {
            matches
                .iter()
                .rev()
                .find(|&&s| s < from)
                .or_else(|| matches.last())
        };
        if let Some(&start) = next {
            self.jump_to_match(start, query.len(), select);
        }
    }

    fn jump_to_match(&mut self, start: usize, len: usize, select: bool) {
        self.history.break_run();
        if select {
            self.selection_anchor = Some(start);
            self.cursor.byte = (start + len).min(self.buffer.len_bytes());
        } else {
            self.selection_anchor = None;
            self.cursor.byte = start.min(self.buffer.len_bytes());
        }
        self.ensure_layout();
        self.cursor.sync_goal(&self.layout);
        self.follow_cursor = true;
    }

    /// Replace the currently selected match and step to the next one; with no
    /// match selected, just find the first.
    fn replace_current(&mut self, query: &str, replacement: &str) {
        let selected = self.selection_range().filter(|(s, _)| {
            self.search_matches(query).contains(s)
                // Guard against a stale selection of a different length.
                && self.selection_range().is_some_and(|(a, b)| b - a == query.len())
        });
        match selected {
            Some((s, e)) => {
                self.history.break_run();
                self.replace_range(s, e, replacement);
                self.history.break_run();
                self.find_step(query, true, true);
            }
            None => self.find_step(query, true, true),
        }
    }

    /// Replace every match in one pass — a single undo step — and keep the
    /// cursor at its position adjusted for the replacements before it.
    fn replace_all(&mut self, query: &str, replacement: &str) -> usize {
        let matches = self.search_matches(query);
        if matches.is_empty() {
            return 0;
        }
        let source = self.buffer.rope().to_string();
        let mut out = String::with_capacity(source.len());
        let mut prev = 0;
        for &m in &matches {
            out.push_str(&source[prev..m]);
            out.push_str(replacement);
            prev = m + query.len();
        }
        out.push_str(&source[prev..]);

        let old_cursor = self.cursor.byte;
        let delta = replacement.len() as isize - query.len() as isize;
        let before = matches
            .iter()
            .take_while(|&&m| m + query.len() <= old_cursor)
            .count();
        let mut new_cursor = old_cursor.saturating_add_signed(before as isize * delta);
        if let Some(&m) = matches.get(before)
            && m < old_cursor
        {
            // The cursor sat inside a match; land just after its replacement.
            new_cursor = m.saturating_add_signed(before as isize * delta) + replacement.len();
        }

        self.history.break_run();
        self.replace_range_with_cursor(0, self.buffer.len_bytes(), &out, Some(new_cursor));
        self.history.break_run();
        matches.len()
    }
}
