use super::App;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Save,
    SaveAs,
    Quit,
    Settings,
    Find,
    TogglePreview,
    ToggleRaw,
    ToggleTable,
    ToggleCursor,
    CyclePreset,
    Undo,
    Redo,
    SelectAll,
    Copy,
    Cut,
    Paste,
}

impl App {
    pub fn action_enabled(&self, action: Action) -> bool {
        match action {
            Action::ToggleTable
            | Action::Undo
            | Action::Redo
            | Action::SelectAll
            | Action::Copy
            | Action::Cut
            | Action::Paste => !self.mode.is_read(),
            _ => true,
        }
    }

    pub fn run_action(&mut self, action: Action) {
        if !self.action_enabled(action) {
            return;
        }
        match action {
            Action::Save => self.save(),
            Action::SaveAs => self.open_save_as_current(),
            Action::Quit => self.request_quit(),
            Action::Settings => self.open_menu(),
            Action::Find => self.open_find(),
            Action::TogglePreview => self.toggle_preview(),
            Action::ToggleRaw => self.toggle_raw_view(),
            Action::ToggleTable => self.toggle_table_mode(),
            Action::ToggleCursor => self.toggle_cursor_shape(),
            Action::CyclePreset => self.cycle_preset(),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::SelectAll => self.select_all(),
            Action::Copy => self.copy(),
            Action::Cut => self.cut(),
            Action::Paste => self.paste(),
        }
    }
}
