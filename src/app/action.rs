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
    Bold,
    Italic,
    Strikethrough,
    InlineCode,
    Link,
    CycleHeading,
    ToggleQuote,
    ToggleBullet,
    ToggleTask,
    Outline,
    FollowLink,
    ToggleStats,
}

impl Action {
    pub const ALL: [Self; 28] = [
        Self::Save,
        Self::SaveAs,
        Self::Find,
        Self::Undo,
        Self::Redo,
        Self::SelectAll,
        Self::Copy,
        Self::Cut,
        Self::Paste,
        Self::Bold,
        Self::Italic,
        Self::Strikethrough,
        Self::InlineCode,
        Self::Link,
        Self::CycleHeading,
        Self::ToggleQuote,
        Self::ToggleBullet,
        Self::ToggleTask,
        Self::Outline,
        Self::FollowLink,
        Self::ToggleStats,
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
            Self::Bold => "Toggle bold",
            Self::Italic => "Toggle italic",
            Self::Strikethrough => "Toggle strikethrough",
            Self::InlineCode => "Toggle inline code",
            Self::Link => "Insert link",
            Self::CycleHeading => "Cycle heading level",
            Self::ToggleQuote => "Toggle block quote",
            Self::ToggleBullet => "Toggle bullet list",
            Self::ToggleTask => "Toggle task item",
            Self::Outline => "Document outline",
            Self::FollowLink => "Follow link or footnote",
            Self::ToggleStats => "Toggle document statistics",
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
            Self::Bold
            | Self::Italic
            | Self::Strikethrough
            | Self::InlineCode
            | Self::Link
            | Self::CycleHeading
            | Self::ToggleQuote
            | Self::ToggleBullet => "",
            Self::ToggleTask => "",
            Self::Outline => "Ctrl+Shift+O",
            Self::FollowLink => "",
            Self::ToggleStats => "",
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
            Action::Bold
            | Action::Italic
            | Action::Strikethrough
            | Action::InlineCode
            | Action::Link
            | Action::CycleHeading
            | Action::ToggleQuote
            | Action::ToggleBullet => !self.mode.is_read(),
            Action::ToggleTask => !self.mode.is_read(),
            Action::Outline => true,
            Action::FollowLink => !self.mode.is_read(),
            Action::ToggleStats => true,
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
            Action::Bold => self.toggle_inline("**"),
            Action::Italic => self.toggle_inline("*"),
            Action::Strikethrough => self.toggle_inline("~~"),
            Action::InlineCode => self.toggle_inline("\u{60}"),
            Action::Link => self.insert_link(),
            Action::CycleHeading => self.cycle_heading(),
            Action::ToggleQuote => self.toggle_line_prefix("> "),
            Action::ToggleBullet => self.toggle_line_prefix("- "),
            Action::ToggleTask => self.toggle_task(),
            Action::Outline => self.open_outline(),
            Action::FollowLink => self.follow_link(),
            Action::ToggleStats => self.toggle_status_stats(),
        }
    }
}
