//! The bottom-line input prompt.
//!
//! Saving an unnamed buffer (e.g. one started empty or piped from stdin) opens a
//! "Save as" prompt at the status line, pre-filled with `.md` and the cursor
//! before the extension so typing a name yields `name.md`. Confirming an
//! existing path asks for an overwrite (`y`/`n`), mirroring how nano guards a
//! write. The actual write is atomic (see [`crate::buffer`]).

use std::path::PathBuf;

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::App;

const SAVE_LABEL: &str = "Save as: ";
const FIND_LABEL: &str = "Find: ";
const REPLACE_LABEL: &str = "Replace with: ";

pub(super) enum Prompt {
    /// Editing a filename for an unnamed buffer. `cursor` is a byte offset into
    /// `input` (always on a grapheme boundary).
    SaveAs {
        input: String,
        cursor: usize,
        error: Option<String>,
    },
    /// Confirming an overwrite of an existing path; `input` is kept so `n` can
    /// return to editing the name.
    Overwrite {
        path: PathBuf,
        input: String,
    },
    /// Confirming a quit while the buffer has unsaved changes.
    ConfirmQuit,
    ExternalChange {
        error: Option<String>,
    },
    /// Incremental find; the current match is shown as the selection.
    Find {
        input: String,
        cursor: usize,
    },
    /// Replace target for the find `query`.
    Replace {
        query: String,
        input: String,
        cursor: usize,
    },
}

/// What the UI needs to draw the active prompt on the status line.
pub struct PromptView {
    pub text: String,
    /// Display column of the input cursor within the line, if the prompt edits text.
    pub cursor_col: Option<u16>,
}

impl App {
    /// Open the "Save as" prompt for an unnamed buffer.
    pub(super) fn open_save_as(&mut self) {
        self.prompt = Some(Prompt::SaveAs {
            input: ".md".to_string(),
            cursor: 0,
            error: None,
        });
    }

    /// Ask before quitting with unsaved changes.
    pub(super) fn open_confirm_quit(&mut self) {
        self.prompt = Some(Prompt::ConfirmQuit);
    }

    /// The active prompt rendered for the status line, if any.
    pub fn prompt_view(&self) -> Option<PromptView> {
        match self.prompt.as_ref()? {
            Prompt::SaveAs {
                input,
                cursor,
                error,
            } => {
                let label = error.as_ref().map_or_else(
                    || SAVE_LABEL.to_string(),
                    |error| format!("{error} · {SAVE_LABEL}"),
                );
                let col = UnicodeWidthStr::width(label.as_str())
                    + UnicodeWidthStr::width(&input[..*cursor]);
                Some(PromptView {
                    text: format!("{label}{input}"),
                    cursor_col: Some(col as u16),
                })
            }
            Prompt::Overwrite { path, .. } => Some(PromptView {
                text: format!("Overwrite {}?  (y/n)", path.display()),
                cursor_col: None,
            }),
            Prompt::ConfirmQuit => Some(PromptView {
                text: "Unsaved changes — quit without saving?  (y/n, w = write first)".to_string(),
                cursor_col: None,
            }),
            Prompt::ExternalChange { error } => {
                let message =
                    "File changed on disk — r reload · o overwrite · a save as · Esc cancel";
                Some(PromptView {
                    text: error.as_ref().map_or_else(
                        || message.to_string(),
                        |error| format!("{error} · {message}"),
                    ),
                    cursor_col: None,
                })
            }
            Prompt::Find { input, cursor } => {
                let mut text = format!("{FIND_LABEL}{input}");
                if !input.is_empty() {
                    let matches = self.search_matches(input);
                    let current = self
                        .selection_range()
                        .and_then(|(s, _)| matches.iter().position(|&m| m == s));
                    text.push_str(&match (current, matches.len()) {
                        (_, 0) => "   no matches".to_string(),
                        (Some(i), n) => format!("   {}/{n}", i + 1),
                        (None, n) => format!("   {n} matches"),
                    });
                }
                text.push_str("   (Enter/\u{2193} next · \u{2191} prev · Tab replace · Esc close)");
                let col =
                    UnicodeWidthStr::width(FIND_LABEL) + UnicodeWidthStr::width(&input[..*cursor]);
                Some(PromptView {
                    text,
                    cursor_col: Some(col as u16),
                })
            }
            Prompt::Replace {
                query,
                input,
                cursor,
            } => {
                let text = format!(
                    "{REPLACE_LABEL}{input}   [\"{query}\"]  (Enter replace · ^A all · Tab back · Esc close)"
                );
                let col = UnicodeWidthStr::width(REPLACE_LABEL)
                    + UnicodeWidthStr::width(&input[..*cursor]);
                Some(PromptView {
                    text,
                    cursor_col: Some(col as u16),
                })
            }
        }
    }

    pub(super) fn handle_prompt_key(&mut self, key: KeyEvent) {
        match self.prompt.take() {
            Some(Prompt::SaveAs {
                input,
                cursor,
                error: _,
            }) => self.save_as_key(key, input, cursor),
            Some(Prompt::Overwrite { path, input }) => self.overwrite_key(key, path, input),
            Some(Prompt::ConfirmQuit) => self.confirm_quit_key(key),
            Some(Prompt::ExternalChange { error: _ }) => self.external_change_key(key),
            Some(Prompt::Find { input, cursor }) => self.find_key(key, input, cursor),
            Some(Prompt::Replace {
                query,
                input,
                cursor,
            }) => self.replace_key(key, query, input, cursor),
            None => {}
        }
    }

    fn confirm_quit_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.should_quit = true,
            KeyCode::Char('w') | KeyCode::Char('W') => {
                // Save first; `save` opens the save-as prompt for an unnamed
                // buffer, in which case the quit is abandoned and the user can
                // ^Q again once the buffer has a name.
                self.save();
                if !self.buffer.modified() && self.prompt.is_none() {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.status = Some("Quit cancelled".to_string());
            }
            _ => self.prompt = Some(Prompt::ConfirmQuit),
        }
    }

    pub(super) fn open_external_change(&mut self) {
        self.prompt = Some(Prompt::ExternalChange { error: None });
    }

    fn external_change_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('r') | KeyCode::Char('R') => match self.reload_buffer() {
                Ok(()) => self.status = Some("Reloaded".to_string()),
                Err(error) => {
                    self.prompt = Some(Prompt::ExternalChange {
                        error: Some(format!("Reload failed: {error}")),
                    });
                }
            },
            KeyCode::Char('o') | KeyCode::Char('O') => self.write_buffer_force(),
            KeyCode::Char('a') | KeyCode::Char('A') => {
                let input = self
                    .buffer
                    .path()
                    .map_or_else(|| ".md".to_string(), |path| path.display().to_string());
                let cursor = input.len();
                self.prompt = Some(Prompt::SaveAs {
                    input,
                    cursor,
                    error: None,
                });
            }
            KeyCode::Esc => self.status = Some("Save cancelled".to_string()),
            _ => self.prompt = Some(Prompt::ExternalChange { error: None }),
        }
    }

    fn save_as_key(&mut self, key: KeyEvent, mut input: String, mut cursor: usize) {
        match key.code {
            KeyCode::Esc => {
                self.status = Some("Save cancelled".to_string());
                return;
            }
            KeyCode::Enter => {
                self.confirm_save_as(&input);
                return;
            }
            _ => {
                edit_line(key, &mut input, &mut cursor);
            }
        }
        self.prompt = Some(Prompt::SaveAs {
            input,
            cursor,
            error: None,
        });
    }

    fn confirm_save_as(&mut self, input: &str) {
        match resolve_save_path(input) {
            Ok(path) if path.is_dir() => {
                self.reopen_save_as(input, format!("{} is a directory", path.display()));
            }
            Ok(path) if path.exists() => {
                self.prompt = Some(Prompt::Overwrite {
                    path,
                    input: input.to_string(),
                });
            }
            Ok(path) => self.save_to(path, input),
            Err(e) => self.reopen_save_as(input, format!("invalid name: {e}")),
        }
    }

    fn overwrite_key(&mut self, key: KeyEvent, path: PathBuf, input: String) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.save_to(path, &input),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                let cursor = input.len();
                self.status = Some("Save cancelled".to_string());
                self.prompt = Some(Prompt::SaveAs {
                    input,
                    cursor,
                    error: None,
                });
            }
            _ => self.prompt = Some(Prompt::Overwrite { path, input }),
        }
    }

    /// Try the save; on failure reopen the prompt with the typed name so the
    /// user can correct it (a failed write must not bind the buffer to the
    /// bad path, or every later ^S would silently retry it).
    fn save_to(&mut self, path: PathBuf, input: &str) {
        match self.buffer.save_as(path) {
            Ok(()) => {
                self.prompt = None;
                self.history.mark_saved();
                self.auto_save_retry_at = None;
                self.status = Some("Saved".to_string());
            }
            Err(e) => self.reopen_save_as(input, format!("Error: {e}")),
        }
    }

    fn reopen_save_as(&mut self, input: &str, message: String) {
        self.prompt = Some(Prompt::SaveAs {
            input: input.to_string(),
            cursor: input.len(),
            error: Some(message),
        });
    }
}

/// Shared single-line text editing for prompts (cursor motion, deletion, and
/// character insertion). Returns whether the key was consumed.
pub(super) fn edit_line(key: KeyEvent, input: &mut String, cursor: &mut usize) -> bool {
    let ctrl = super::ctrl_like(key.modifiers);
    match key.code {
        KeyCode::Left => *cursor = prev_grapheme(input, *cursor),
        KeyCode::Right => *cursor = next_grapheme(input, *cursor),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = input.len(),
        KeyCode::Backspace if *cursor > 0 => {
            let prev = prev_grapheme(input, *cursor);
            input.replace_range(prev..*cursor, "");
            *cursor = prev;
        }
        KeyCode::Delete if *cursor < input.len() => {
            let next = next_grapheme(input, *cursor);
            input.replace_range(*cursor..next, "");
        }
        KeyCode::Char(c) if !ctrl => {
            input.insert(*cursor, c);
            *cursor += c.len_utf8();
        }
        _ => return false,
    }
    true
}

fn prev_grapheme(s: &str, at: usize) -> usize {
    s[..at]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}

fn next_grapheme(s: &str, at: usize) -> usize {
    s[at..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(s.len(), |(i, _)| at + i)
}

/// Resolve the typed filename to a path: trim surrounding whitespace, reject an
/// empty name, and expand a leading `~`. Relative paths stay relative and the OS
/// resolves them against the working directory at write time.
fn resolve_save_path(input: &str) -> Result<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("empty filename");
    }
    let home_rest = trimmed
        .strip_prefix("~/")
        .or_else(|| (trimmed == "~").then_some(""));
    if let Some(rest) = home_rest
        && let Some(base) = directories::BaseDirs::new()
    {
        let home = base.home_dir();
        return Ok(if rest.is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        });
    }
    Ok(PathBuf::from(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_resolves_relative() {
        assert!(resolve_save_path("   ").is_err());
        assert_eq!(
            resolve_save_path(" notes.md ").unwrap(),
            PathBuf::from("notes.md")
        );
    }

    #[test]
    fn expands_leading_tilde() {
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        assert_eq!(resolve_save_path("~/a/b.md").unwrap(), home.join("a/b.md"));
        assert_eq!(resolve_save_path("~").unwrap(), home);
    }
}
