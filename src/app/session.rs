use super::palette::fuzzy_score;
use super::{App, History};
use crate::buffer::TextBuffer;
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use unicode_segmentation::UnicodeSegmentation;

const MAX_RECENT_FILES: usize = 20;

#[derive(Clone, Serialize, Deserialize)]
pub struct RecentFile {
    pub(super) path: String,
    cursor: usize,
    focus_scroll: usize,
    raw_scroll: usize,
    read_scroll: usize,
}

#[derive(Default, Serialize, Deserialize)]
struct SessionFile {
    recent: Vec<RecentFile>,
}

pub struct RecentPicker {
    query: String,
    selected: usize,
}

pub struct RecentItem {
    pub path: String,
    pub selected: bool,
}

impl App {
    pub fn load_session(&mut self) -> Result<()> {
        let Some(path) = session_path() else {
            return Ok(());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let state: SessionFile = toml::from_str(&text)?;
        self.recent_files = state
            .recent
            .into_iter()
            .filter(|entry| Path::new(&entry.path).is_file())
            .take(MAX_RECENT_FILES)
            .collect();
        if let Some(current) = self.buffer.path().map(path_key)
            && let Some(entry) = self.recent_files.iter().find(|entry| entry.path == current)
        {
            self.cursor.byte = entry.cursor.min(self.buffer.len_bytes());
            self.focus_scroll_y = entry.focus_scroll;
            self.raw_scroll_y = entry.raw_scroll;
            self.read_scroll_y = entry.read_scroll;
            self.scroll_y = self.focus_scroll_y;
        }
        Ok(())
    }

    pub fn save_session(&mut self) -> Result<()> {
        self.remember_current_file();
        let Some(path) = session_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let state = SessionFile {
            recent: self.recent_files.clone(),
        };
        let text = toml::to_string(&state)?;
        crate::buffer::write_atomic(&path, &ropey::Rope::from_str(&text))
    }

    pub(super) fn open_recent_files(&mut self) {
        if self.buffer.modified() {
            self.status = Some("Save or discard changes before switching files".to_string());
            return;
        }
        if self.recent_files.is_empty() {
            self.status = Some("No recent files".to_string());
            return;
        }
        self.palette = None;
        self.recent_picker = Some(RecentPicker {
            query: String::new(),
            selected: 0,
        });
    }

    pub fn recent_picker_open(&self) -> bool {
        self.recent_picker.is_some()
    }

    pub fn recent_query(&self) -> &str {
        self.recent_picker
            .as_ref()
            .map_or("", |picker| picker.query.as_str())
    }

    pub fn recent_items(&self) -> Vec<RecentItem> {
        let Some(picker) = self.recent_picker.as_ref() else {
            return Vec::new();
        };
        picker
            .matches(&self.recent_files)
            .into_iter()
            .enumerate()
            .map(|(index, file)| RecentItem {
                path: file.path.clone(),
                selected: index == picker.selected,
            })
            .collect()
    }

    pub(super) fn handle_recent_key(&mut self, key: KeyEvent) {
        let Some(mut picker) = self.recent_picker.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Up => picker.move_selection(&self.recent_files, -1),
            KeyCode::Down => picker.move_selection(&self.recent_files, 1),
            KeyCode::Enter => {
                if let Some(entry) = picker
                    .matches(&self.recent_files)
                    .get(picker.selected)
                    .map(|entry| (*entry).clone())
                {
                    match self.open_recent_entry(&entry) {
                        Ok(()) => {
                            self.status = Some("Opened recent file".to_string());
                            return;
                        }
                        Err(error) => self.status = Some(format!("Could not open file: {error}")),
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some((index, _)) = picker.query.grapheme_indices(true).next_back() {
                    picker.query.truncate(index);
                    picker.selected = 0;
                }
            }
            KeyCode::Char(ch) if !super::ctrl_like(key.modifiers) => {
                picker.query.push(ch);
                picker.selected = 0;
            }
            _ => {}
        }
        self.recent_picker = Some(picker);
    }

    fn open_recent_entry(&mut self, entry: &RecentFile) -> Result<()> {
        self.remember_current_file();
        self.buffer = TextBuffer::from_path(&entry.path)?;
        self.history = History::new();
        self.reset_after_buffer_change();
        self.cursor.byte = entry.cursor.min(self.buffer.len_bytes());
        self.focus_scroll_y = entry.focus_scroll;
        self.raw_scroll_y = entry.raw_scroll;
        self.read_scroll_y = entry.read_scroll;
        self.scroll_y = if self.raw_view {
            self.raw_scroll_y
        } else {
            self.focus_scroll_y
        };
        self.ensure_layout();
        self.cursor.sync_goal(&self.layout);
        self.offer_recovery();
        Ok(())
    }

    pub(super) fn remember_current_file(&mut self) {
        let Some(path) = self.buffer.path().map(path_key) else {
            return;
        };
        self.save_edit_scroll();
        self.recent_files.retain(|entry| entry.path != path);
        self.recent_files.insert(
            0,
            RecentFile {
                path,
                cursor: self.cursor.byte,
                focus_scroll: self.focus_scroll_y,
                raw_scroll: self.raw_scroll_y,
                read_scroll: self.read_scroll_y,
            },
        );
        self.recent_files.truncate(MAX_RECENT_FILES);
    }
}

impl RecentPicker {
    fn matches<'a>(&self, files: &'a [RecentFile]) -> Vec<&'a RecentFile> {
        if self.query.is_empty() {
            return files.iter().collect();
        }
        let mut matches: Vec<_> = files
            .iter()
            .filter_map(|file| fuzzy_score(&file.path, &self.query).map(|score| (score, file)))
            .collect();
        matches.sort_by(|(left_score, _), (right_score, _)| right_score.cmp(left_score));
        matches.into_iter().map(|(_, file)| file).collect()
    }

    fn move_selection(&mut self, files: &[RecentFile], delta: isize) {
        let len = self.matches(files).len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.saturating_add_signed(delta).min(len - 1);
        }
    }
}

fn session_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "marqi")
        .map(|dirs| dirs.data_local_dir().join("session.toml"))
}

pub(super) fn path_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .or_else(|_| {
            if path.is_absolute() {
                Ok(path.to_path_buf())
            } else {
                std::env::current_dir().map(|dir| dir.join(path))
            }
        })
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn recent_picker_filters_paths() {
        let files = vec![
            RecentFile {
                path: "/notes/alpha.md".to_string(),
                cursor: 0,
                focus_scroll: 0,
                raw_scroll: 0,
                read_scroll: 0,
            },
            RecentFile {
                path: "/notes/beta.md".to_string(),
                cursor: 0,
                focus_scroll: 0,
                raw_scroll: 0,
                read_scroll: 0,
            },
        ];
        let picker = RecentPicker {
            query: "alp".to_string(),
            selected: 0,
        };
        let matches = picker.matches(&files);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, "/notes/alpha.md");
    }

    #[test]
    fn recent_entry_restores_file_and_cursor() {
        let first =
            std::env::temp_dir().join(format!("marqi_session_first_{}.md", std::process::id()));
        let second =
            std::env::temp_dir().join(format!("marqi_session_second_{}.md", std::process::id()));
        std::fs::write(&first, "first").unwrap();
        std::fs::write(&second, "second").unwrap();
        let mut app = App::with_config(TextBuffer::from_path(&first).unwrap(), &Config::default());
        let entry = RecentFile {
            path: second.to_string_lossy().into_owned(),
            cursor: 3,
            focus_scroll: 0,
            raw_scroll: 0,
            read_scroll: 0,
        };

        app.open_recent_entry(&entry).unwrap();

        assert_eq!(app.buffer.rope().to_string(), "second");
        assert_eq!(app.cursor.byte, 3);
        std::fs::remove_file(first).ok();
        std::fs::remove_file(second).ok();
    }
}
