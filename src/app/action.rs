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
    CommandPalette,
}

impl Action {
    pub const ALL: [Self; 16] = [
        Self::Save,
        Self::SaveAs,
        Self::Find,
        Self::Undo,
        Self::Redo,
        Self::SelectAll,
        Self::Copy,
        Self::Cut,
        Self::Paste,
        Self::TogglePreview,
        Self::ToggleRaw,
        Self::ToggleTable,
        Self::Settings,
        Self::ToggleCursor,
        Self::CyclePreset,
        Self::Quit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Save => "Save",
            Self::SaveAs => "Save as",
            Self::Quit => "Quit",
            Self::Settings => "Settings",
            Self::Find => "Find and replace",
            Self::TogglePreview => "Toggle preview",
            Self::ToggleRaw => "Toggle raw source",
            Self::ToggleTable => "Toggle table mode",
            Self::ToggleCursor => "Toggle cursor shape",
            Self::CyclePreset => "Cycle keybindings",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::SelectAll => "Select all",
            Self::Copy => "Copy",
            Self::Cut => "Cut",
            Self::Paste => "Paste",
            Self::CommandPalette => "Command palette",
        }
    }

    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Save => "Ctrl+S",
            Self::SaveAs => "Ctrl+Shift+S",
            Self::Quit => "Ctrl+Q",
            Self::Settings => "Ctrl+G",
            Self::Find => "Ctrl+F",
            Self::TogglePreview => "Ctrl+P",
            Self::ToggleRaw => "Ctrl+R",
            Self::ToggleTable => "Ctrl+T",
            Self::ToggleCursor => "Ctrl+B",
            Self::CyclePreset => "Ctrl+L",
            Self::Undo => "Ctrl+Z",
            Self::Redo => "Ctrl+Y",
            Self::SelectAll => "Ctrl+A",
            Self::Copy => "Ctrl+C",
            Self::Cut => "Ctrl+X",
            Self::Paste => "Ctrl+V",
            Self::CommandPalette => "Ctrl+Shift+P",
        }
    }
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
            Action::CommandPalette => self.open_palette(),
        }
    }
}
