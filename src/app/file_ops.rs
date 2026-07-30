use super::{App, History};
use crate::buffer::TextBuffer;
use std::path::PathBuf;

impl App {
    pub(super) fn new_document(&mut self) {
        if self.buffer.modified() {
            self.status = Some("Save or discard changes before creating a document".to_string());
            return;
        }
        self.remember_current_file();
        self.buffer = TextBuffer::empty();
        self.history = History::new();
        self.reset_after_buffer_change();
        self.cursor.byte = 0;
        self.reset_file_view();
        self.status = Some("New document".to_string());
    }

    pub(super) fn open_file_path(&mut self, path: PathBuf) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.buffer.modified(),
            "save or discard changes before switching files"
        );
        self.remember_current_file();
        self.buffer = TextBuffer::from_path(path)?;
        self.history = History::new();
        self.reset_after_buffer_change();
        self.cursor.byte = 0;
        self.reset_file_view();
        self.offer_recovery();
        Ok(())
    }

    pub(super) fn rename_file_path(&mut self, path: PathBuf) -> anyhow::Result<()> {
        anyhow::ensure!(!self.buffer.modified(), "save changes before renaming");
        self.buffer.rename_to(path)?;
        self.status = Some("File renamed".to_string());
        Ok(())
    }

    pub(super) fn trash_current_file(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.buffer.modified(),
            "save changes before moving to trash"
        );
        let path = self
            .buffer
            .path()
            .ok_or_else(|| anyhow::anyhow!("document has no file name"))?
            .to_path_buf();
        trash::delete(&path)?;
        self.recent_files
            .retain(|entry| entry.path != path.to_string_lossy());
        self.buffer = TextBuffer::empty();
        self.history = History::new();
        self.reset_after_buffer_change();
        self.cursor.byte = 0;
        self.reset_file_view();
        self.status = Some("File moved to trash".to_string());
        Ok(())
    }

    fn reset_file_view(&mut self) {
        self.scroll_y = 0;
        self.focus_scroll_y = 0;
        self.raw_scroll_y = 0;
        self.read_scroll_y = 0;
        self.mode = self.resting_mode();
        self.follow_cursor = true;
    }
}
