//! Find and replace.
//!
//! `open_find` opens an incremental find prompt: typing jumps to the first
//! match at or after where the search started, Enter/↓ and ↑ step through
//! matches (wrapping), and Tab switches to a replace prompt where Enter
//! replaces the current match and `^A` replaces every match as one undo step.
//! The current match is shown as a selection, so it is visible in any view.
//!
//! Matching defaults to Unicode smart case. Case-sensitive, whole-word, regex,
//! and selection-only searches can be toggled while the prompt is open.

use super::App;
use super::prompt::Prompt;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use regex::RegexBuilder;
use std::rc::Rc;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub selection_only: bool,
}

#[derive(Clone, Copy)]
pub(super) struct FindRestore {
    pub cursor: usize,
    pub selection_anchor: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SearchMatch {
    pub start: usize,
    pub end: usize,
}

#[derive(Default)]
pub(super) struct SearchCache {
    version: u64,
    query: String,
    options: SearchOptions,
    scope: Option<(usize, usize)>,
    matches: Rc<[SearchMatch]>,
    error: Option<String>,
    valid: bool,
    pub(super) scans: usize,
}

impl App {
    /// Open the find prompt, seeded with the current selection (if it is a
    /// reasonable query) or the previous search.
    pub(super) fn open_find(&mut self) {
        let original_selection = self.selection_range();
        let seed = self
            .selection_range()
            .map(|(s, e)| self.buffer.slice(s, e))
            .filter(|t| !t.is_empty() && !t.contains('\n') && t.len() <= 200)
            .unwrap_or_else(|| self.last_search.clone());
        self.find_origin = original_selection
            .map(|(s, _)| s)
            .unwrap_or(self.cursor.byte);
        self.find_restore = Some(FindRestore {
            cursor: self.cursor.byte,
            selection_anchor: self.selection_anchor,
        });
        self.find_scope = original_selection;
        self.search_options.selection_only = false;
        self.prompt = Some(Prompt::Find {
            cursor: seed.len(),
            input: seed,
        });
    }

    pub(super) fn find_key(&mut self, key: KeyEvent, mut input: String, mut cursor: usize) {
        if self.toggle_search_option(key, &input) {
            self.prompt = Some(Prompt::Find { input, cursor });
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.last_search = input;
                self.cancel_find();
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
        key: KeyEvent,
        query: String,
        mut input: String,
        mut cursor: usize,
    ) {
        let ctrl = super::ctrl_like(key.modifiers);
        if self.toggle_search_option(key, &query) {
            self.prompt = Some(Prompt::Replace {
                query,
                input,
                cursor,
            });
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.last_search = query;
                self.cancel_find();
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
                self.finish_find();
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

    pub(super) fn search_matches(&self, query: &str) -> Rc<[SearchMatch]> {
        if query.is_empty() {
            return Rc::from([]);
        }
        let scope = self
            .search_options
            .selection_only
            .then_some(self.find_scope)
            .flatten();
        {
            let cache = self.search_cache.borrow();
            if cache.valid
                && cache.version == self.version
                && cache.query == query
                && cache.options == self.search_options
                && cache.scope == scope
            {
                return Rc::clone(&cache.matches);
            }
        }
        let source = self.buffer.rope().to_string();
        let pattern = if self.search_options.regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        let exact = self.search_options.case_sensitive
            || (!self.search_options.regex && query.chars().any(char::is_uppercase));
        let compiled = RegexBuilder::new(&pattern)
            .case_insensitive(!exact)
            .unicode(true)
            .build();
        let (matches, error) = match compiled {
            Ok(regex) => {
                let matches = regex
                    .find_iter(&source)
                    .filter(|found| found.start() < found.end())
                    .filter(|found| {
                        scope
                            .is_none_or(|(start, end)| found.start() >= start && found.end() <= end)
                    })
                    .filter(|found| {
                        !self.search_options.whole_word
                            || whole_word_match(&source, found.start(), found.end())
                    })
                    .map(|found| SearchMatch {
                        start: found.start(),
                        end: found.end(),
                    })
                    .collect::<Vec<_>>();
                (Rc::from(matches), None)
            }
            Err(error) => (Rc::from([]), Some(error.to_string())),
        };
        let mut cache = self.search_cache.borrow_mut();
        cache.version = self.version;
        cache.query.clear();
        cache.query.push_str(query);
        cache.options = self.search_options;
        cache.scope = scope;
        cache.matches = Rc::clone(&matches);
        cache.error = error;
        cache.valid = true;
        cache.scans += 1;
        matches
    }

    pub(super) fn search_error(&self) -> Option<String> {
        self.search_cache.borrow().error.clone()
    }

    /// Jump to the first match at or after where the search started (used
    /// while typing the query). Wraps to the first match in the document.
    fn find_from_origin(&mut self, query: &str) {
        let matches = self.search_matches(query);
        let Some(found) = matches
            .iter()
            .find(|found| found.start >= self.find_origin)
            .or_else(|| matches.first())
        else {
            self.selection_anchor = None;
            return;
        };
        self.jump_to_match(*found, true);
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
        let on_match = matches.iter().any(|found| found.start == from);
        let next = if forward {
            matches
                .iter()
                .find(|found| {
                    if on_match {
                        found.start > from
                    } else {
                        found.start >= from
                    }
                })
                .or_else(|| matches.first())
        } else {
            matches
                .iter()
                .rev()
                .find(|found| found.start < from)
                .or_else(|| matches.last())
        };
        if let Some(found) = next {
            self.jump_to_match(*found, select);
        }
    }

    fn jump_to_match(&mut self, found: SearchMatch, select: bool) {
        self.history.break_run();
        if select {
            self.selection_anchor = Some(found.start);
            self.cursor.byte = found.end.min(self.buffer.len_bytes());
        } else {
            self.selection_anchor = None;
            self.cursor.byte = found.start.min(self.buffer.len_bytes());
        }
        self.ensure_layout();
        self.cursor.sync_goal(&self.layout);
        self.follow_cursor = true;
    }

    /// Replace the currently selected match and step to the next one; with no
    /// match selected, just find the first.
    fn replace_current(&mut self, query: &str, replacement: &str) {
        let selection = self.selection_range();
        let selected = selection.and_then(|(start, end)| {
            self.search_matches(query)
                .iter()
                .find(|found| found.start == start && found.end == end)
                .copied()
        });
        match selected {
            Some(found) => {
                self.history.break_run();
                self.replace_range(found.start, found.end, replacement);
                self.history.break_run();
                if let Some((_, end)) = self.find_scope.as_mut()
                    && found.end <= *end
                {
                    *end = *end + replacement.len() - (found.end - found.start);
                }
                self.find_restore = None;
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
        for found in matches.iter() {
            out.push_str(&source[prev..found.start]);
            out.push_str(replacement);
            prev = found.end;
        }
        out.push_str(&source[prev..]);

        let old_cursor = self.cursor.byte;
        let new_cursor = cursor_after_replacements(old_cursor, &matches, replacement.len());

        self.history.break_run();
        self.replace_range_with_cursor(0, self.buffer.len_bytes(), &out, Some(new_cursor));
        self.history.break_run();
        self.find_restore = None;
        matches.len()
    }

    fn toggle_search_option(&mut self, key: KeyEvent, query: &str) -> bool {
        if !key.modifiers.contains(KeyModifiers::ALT) {
            return false;
        }
        match key.code {
            KeyCode::Char('c' | 'C') => {
                self.search_options.case_sensitive = !self.search_options.case_sensitive
            }
            KeyCode::Char('w' | 'W') => {
                self.search_options.whole_word = !self.search_options.whole_word
            }
            KeyCode::Char('r' | 'R') => self.search_options.regex = !self.search_options.regex,
            KeyCode::Char('s' | 'S') => {
                self.search_options.selection_only =
                    self.find_scope.is_some() && !self.search_options.selection_only
            }
            _ => return false,
        }
        self.find_from_origin(query);
        true
    }

    fn cancel_find(&mut self) {
        if let Some(restore) = self.find_restore.take() {
            self.cursor.byte = restore.cursor.min(self.buffer.len_bytes());
            self.selection_anchor = restore
                .selection_anchor
                .map(|anchor| anchor.min(self.buffer.len_bytes()));
            self.ensure_layout();
            self.cursor.sync_goal(&self.layout);
            self.follow_cursor = true;
        }
        self.find_scope = None;
        self.search_options.selection_only = false;
    }

    fn finish_find(&mut self) {
        self.find_restore = None;
        self.find_scope = None;
        self.search_options.selection_only = false;
    }
}

fn whole_word_match(source: &str, start: usize, end: usize) -> bool {
    let before = source[..start].chars().next_back();
    let after = source[end..].chars().next();
    before.is_none_or(|ch| !is_word_char(ch)) && after.is_none_or(|ch| !is_word_char(ch))
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn cursor_after_replacements(
    cursor: usize,
    matches: &[SearchMatch],
    replacement_len: usize,
) -> usize {
    let mut delta = 0isize;
    for found in matches {
        if cursor < found.start {
            break;
        }
        if cursor < found.end {
            return found
                .start
                .saturating_add_signed(delta)
                .saturating_add(replacement_len);
        }
        delta += replacement_len as isize - (found.end - found.start) as isize;
    }
    cursor.saturating_add_signed(delta)
}
