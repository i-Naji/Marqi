//! Application state and input handling.
//!
//! The editor supports four keybinding presets selected by config: standard and
//! nano/emacs are modeless (printable keys insert; navigation/editing keys move
//! and delete), while vim is modal (Normal/Insert/Visual). `^S` saves, `^Q`
//! quits, and `^L` cycles presets. The app owns the buffer, cursor, a cached
//! display [`Layout`], and a viewport that scrolls to follow the cursor.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use unicode_width::UnicodeWidthStr;

use crate::buffer::TextBuffer;
use crate::clipboard::Clipboard;
use crate::config::Config;
use crate::cursor::Cursor;
use crate::history::{Edit, History};
use crate::layout::Layout;
use crate::line_index::LineIndex;
use crate::markdown::{CodeHighlighter, MarkdownTheme, ThemeName, ThemeVariant};
use crate::text::{next_grapheme, prev_grapheme};
use crate::view::{self, HybridView, PreviewView, ViewCache};

mod action;
mod file_ops;
mod formatting;
mod menu;
mod navigation;
mod outline;
mod palette;
mod prompt;
mod search;
mod session;
mod smart_edit;
mod table;

pub use action::Action;

#[cfg(test)]
mod tests;

const DEFAULT_TAB_WIDTH: usize = 4;
const DEFAULT_SCROLLOFF: usize = 3;
/// Idle time after the last edit before an enabled auto-save fires.
const AUTO_SAVE_DELAY: Duration = Duration::from_secs(2);
const AUTO_SAVE_RETRY_DELAY: Duration = Duration::from_secs(10);

/// Whether the event's modifiers count as "Ctrl" for shortcuts. SUPER (Cmd on
/// macOS, reported by kitty-protocol terminals) is accepted as an alias so
/// Cmd+S etc. work where the terminal passes Cmd through; plain Ctrl works
/// everywhere.
fn ctrl_like(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::CONTROL) || modifiers.contains(KeyModifiers::SUPER)
}

/// The editor's input mode. Standard/nano/emacs rest in `Insert`; vim rests in
/// `Normal` and adds `Visual`; `Read` is the scroll-only rendered preview (^P).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Vim normal mode (commands/motions).
    Normal,
    /// Inserting text (also the resting mode for nano/emacs presets).
    Insert,
    /// Vim visual mode (selection via motions).
    Visual,
    /// Rendered preview, scroll-only.
    Read,
}

impl Mode {
    pub fn is_read(self) -> bool {
        self == Mode::Read
    }
}

/// Keybinding preset, selected by config.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    Standard,
    Vim,
    Nano,
    Emacs,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineNumbers {
    Off,
    Absolute,
    Relative,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Block,
    Line,
}

impl CursorShape {
    pub fn label(self) -> &'static str {
        match self {
            CursorShape::Block => "block",
            CursorShape::Line => "line",
        }
    }
}

/// A cursor motion, shared by every preset.
#[derive(Clone, Copy)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    WordForward,
    WordBack,
    DocStart,
    DocEnd,
    PageUp,
    PageDown,
}

#[derive(Clone, Copy)]
struct EditImpact {
    start_line: usize,
    old_line_count: usize,
    /// Where the replacement text lands; the new line count is derived from
    /// the post-mutation rope (counting inserted `\n`s would miss the other
    /// line breaks ropey recognizes — `\r`, VT, FF, NEL, LS, PS).
    start_byte: usize,
    inserted_len: usize,
    removed_len: usize,
}

/// Wall-clock durations of the most recent expensive pipeline steps, for the
/// `MARQI_STATS` overlay. Zero means "not run yet this session".
#[derive(Default, Clone, Copy)]
pub struct DebugTimings {
    pub last_layout_us: u128,
    /// Last whole-document block partition, copied from [`ViewCacheStats`].
    pub last_parse_us: u128,
    /// Last hybrid/raw/preview structure build.
    pub last_view_build_us: u128,
    /// Last viewport assembly (rendering the visible window's rows).
    pub last_assemble_us: u128,
}

/// Whether `MARQI_STATS` debug counters should be displayed (status-bar
/// segment + exit dump). Read once; flipping the variable mid-session is not
/// supported.
pub fn stats_enabled() -> bool {
    static ENABLED: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("MARQI_STATS").is_some_and(|v| v != "0"));
    *ENABLED
}

pub struct App {
    pub buffer: TextBuffer,
    pub cursor: Cursor,
    pub scroll_y: usize,
    focus_scroll_y: usize,
    raw_scroll_y: usize,
    read_scroll_y: usize,
    read_cursor: usize,
    pub should_quit: bool,
    pub status: Option<String>,
    pub mode: Mode,
    /// The interactive settings popup (theme / appearance / keybindings), when
    /// open. Captures all input while active, like [`prompt`].
    menu: Option<menu::Menu>,
    palette: Option<palette::Palette>,
    outline: Option<outline::Outline>,
    recent_picker: Option<session::RecentPicker>,
    recent_files: Vec<session::RecentFile>,
    cursor_shape: CursorShape,
    table_mode: bool,
    /// Show every line as highlighted source (no markers stripped).
    raw_view: bool,
    preset: Preset,
    line_numbers: LineNumbers,
    /// Leader key of a pending multi-key command (Vim `d`/`g`/`y`, Emacs `C-x`).
    pending: Option<char>,
    /// Active bottom-line prompt (e.g. "save as"), if any.
    prompt: Option<prompt::Prompt>,

    /// Selection anchor (byte offset); the selection runs to the cursor.
    selection_anchor: Option<usize>,
    history: History,
    clipboard: Clipboard,

    /// The last find query, reused by find-next (Vim `n`/`N`) and the prompt.
    last_search: String,
    /// Where the current find session started; incremental typing re-searches
    /// from here instead of walking away down the file.
    find_origin: usize,
    search_cache: RefCell<search::SearchCache>,
    search_options: search::SearchOptions,
    find_restore: Option<search::FindRestore>,
    find_scope: Option<(usize, usize)>,

    /// Save automatically after an idle pause (config `editor.auto_save`).
    auto_save: bool,
    show_status_stats: bool,
    /// When the buffer was last edited, for the auto-save idle check.
    last_edit: Option<Instant>,
    auto_save_retry_at: Option<Instant>,
    recovery_content: Option<String>,
    recovery_written_version: u64,

    /// Whether drawing should keep the cursor in view. Cleared by wheel
    /// scrolling so the user can read elsewhere; any keypress restores it.
    follow_cursor: bool,
    /// Byte where the left mouse button went down (drag-select origin).
    mouse_press_byte: Option<usize>,

    tab_width: usize,
    scrolloff: usize,
    /// Blank columns left of the editor content (config `editor.left_margin`).
    left_margin: usize,

    // Viewport geometry, refreshed by the UI layer each draw.
    wrap_width: usize,
    viewport_height: usize,
    /// Columns left of the content area (margin plus line-number gutter) as
    /// drawn last frame; mouse clicks subtract it to find the content column.
    left_offset: usize,

    // Cached raw layout plus the width it was built for and a dirty flag.
    layout: Layout,
    layout_width: usize,
    layout_dirty: bool,

    /// Per-line tokenizer state, maintained incrementally on every edit
    /// (width-independent, so never "dirty").
    line_index: LineIndex,

    // Monotonic content version, bumped on every edit.
    version: u64,

    // Markdown rendering: shared theme + highlighter and the cached read-mode
    // row index.
    theme: MarkdownTheme,
    highlighter: CodeHighlighter,
    /// Per-element colour overrides from config, re-applied on a runtime theme
    /// switch (the settings popup) so they survive a palette change.
    theme_overrides: HashMap<String, String>,
    /// Explicit syntect theme from config (`theme.syntax`), or `None` to follow
    /// each palette's default code-block pairing.
    syntax_override: Option<String>,
    config_path: Option<std::path::PathBuf>,
    preview_view: Option<PreviewView>,
    preview_width: usize,
    preview_dirty: bool,

    // Cached hybrid (focus-mode) row index and the inputs it was built for.
    // Selection and cursor styling are assembly inputs, not build inputs, so
    // they are deliberately absent here.
    view: Option<HybridView>,
    view_cache: ViewCache,
    view_version: u64,
    view_width: usize,

    /// Most recent pipeline timings (see [`DebugTimings`]).
    timings: DebugTimings,
}

impl App {
    pub fn new(buffer: TextBuffer) -> Self {
        let layout = Layout::build(buffer.rope(), 1, DEFAULT_TAB_WIDTH);
        let line_index = LineIndex::build(buffer.rope());
        Self {
            buffer,
            cursor: Cursor::default(),
            scroll_y: 0,
            focus_scroll_y: 0,
            raw_scroll_y: 0,
            read_scroll_y: 0,
            read_cursor: 0,
            should_quit: false,
            status: None,
            mode: Mode::Insert,
            menu: None,
            palette: None,
            outline: None,
            recent_picker: None,
            recent_files: Vec::new(),
            cursor_shape: CursorShape::Block,
            table_mode: false,
            raw_view: false,
            preset: Preset::Standard,
            line_numbers: LineNumbers::Off,
            pending: None,
            prompt: None,
            selection_anchor: None,
            history: History::new(),
            clipboard: Clipboard::new(),
            last_search: String::new(),
            find_origin: 0,
            search_cache: RefCell::new(search::SearchCache::default()),
            search_options: search::SearchOptions::default(),
            find_restore: None,
            find_scope: None,
            auto_save: false,
            show_status_stats: false,
            last_edit: None,
            auto_save_retry_at: None,
            recovery_content: None,
            recovery_written_version: 0,
            follow_cursor: true,
            mouse_press_byte: None,
            tab_width: DEFAULT_TAB_WIDTH,
            scrolloff: DEFAULT_SCROLLOFF,
            left_margin: 1,
            wrap_width: 1,
            viewport_height: 0,
            left_offset: 0,
            layout,
            layout_width: 1,
            layout_dirty: true,
            line_index,
            version: 0,
            theme: MarkdownTheme::default(),
            highlighter: CodeHighlighter::new(None),
            theme_overrides: HashMap::new(),
            syntax_override: None,
            config_path: None,
            preview_view: None,
            preview_width: 0,
            preview_dirty: true,
            view: None,
            view_cache: ViewCache::default(),
            view_version: 0,
            view_width: 0,
            timings: DebugTimings::default(),
        }
    }

    /// Build an app with user configuration applied (preset, tab width,
    /// scrolloff, syntax theme, and markdown colour overrides).
    pub fn with_config(buffer: TextBuffer, config: &Config) -> Self {
        let mut app = Self::new(buffer);
        app.preset = match config
            .editor
            .keybindings
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "standard" => Preset::Standard,
            "vim" => Preset::Vim,
            "nano" => Preset::Nano,
            "emacs" => Preset::Emacs,
            _ => Preset::Standard,
        };
        app.line_numbers = match config
            .editor
            .line_numbers
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "absolute" | "on" | "true" => LineNumbers::Absolute,
            "relative" => LineNumbers::Relative,
            _ => LineNumbers::Off,
        };
        app.mode = app.resting_mode();
        app.tab_width = config.editor.tab_width.max(1);
        app.scrolloff = config.editor.scrolloff;
        app.left_margin = config.editor.left_margin;
        app.auto_save = config.editor.auto_save;
        app.show_status_stats = config.editor.status_stats;
        app.theme = MarkdownTheme::select(&config.theme.name, &config.theme.variant);
        app.theme.heading_glyphs = config.editor.heading_glyphs;
        app.theme.hard_breaks = config
            .editor
            .soft_break
            .trim()
            .eq_ignore_ascii_case("break");
        app.theme.apply_overrides(&config.theme.markdown);
        // Remembered so the settings popup can re-apply them across a runtime
        // palette switch.
        app.theme_overrides = config.theme.markdown.clone();
        app.syntax_override = config.theme.syntax.clone();
        app.config_path = config.source_path().map(std::path::Path::to_path_buf);
        let syntax = config
            .theme
            .syntax
            .as_deref()
            .unwrap_or_else(|| app.theme.default_syntax_theme());
        app.highlighter = CodeHighlighter::new(Some(syntax));
        app.layout_dirty = true;
        app.preview_dirty = true;
        app
    }

    // --- accessors used by the UI layer ---

    pub fn view(&self) -> &HybridView {
        self.view.as_ref().expect("view built before access")
    }

    /// Screen position of the cursor in the hybrid view: `(column, row)`.
    pub fn cursor_screen(&self) -> (u16, usize) {
        let (layout_row, col) = self.layout.byte_to_pos(self.cursor.byte);
        (col, self.view().cursor_screen_row(layout_row))
    }

    pub fn preset_label(&self) -> &'static str {
        match self.preset {
            Preset::Standard => "standard",
            Preset::Vim => "vim",
            Preset::Nano => "nano",
            Preset::Emacs => "emacs",
        }
    }

    /// The active keybinding preset (for the settings popup's guide filter).
    pub fn preset(&self) -> Preset {
        self.preset
    }

    /// Whether the settings popup is open.
    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// The focused settings row (0 = keybindings, 1 = theme, 2 = appearance).
    pub fn menu_focus(&self) -> usize {
        self.menu.as_ref().map_or(0, |m| m.focus)
    }

    /// Scroll offset into the popup's keybinding guide.
    pub fn menu_guide_scroll(&self) -> usize {
        self.menu.as_ref().map_or(0, |m| m.guide_scroll)
    }

    pub fn line_numbers(&self) -> LineNumbers {
        self.line_numbers
    }

    pub fn left_margin(&self) -> usize {
        self.left_margin
    }

    pub fn status_stats_text(&self) -> Option<String> {
        if !self.show_status_stats {
            return None;
        }
        let mut words = 0usize;
        let mut chars = 0usize;
        let mut in_word = false;
        for ch in self.buffer.rope().chars() {
            chars += 1;
            if ch.is_whitespace() {
                in_word = false;
            } else if !in_word {
                words += 1;
                in_word = true;
            }
        }
        let minutes = words.div_ceil(200).max(1);
        let selection = self
            .selection_range()
            .map(|(start, end)| self.buffer.slice(start, end).chars().count());
        Some(match selection {
            Some(selected) => format!("{words}w · {chars}c · {minutes}m · {selected} selected"),
            None => format!("{words}w · {chars}c · {minutes}m"),
        })
    }

    pub(super) fn toggle_status_stats(&mut self) {
        self.show_status_stats = !self.show_status_stats;
        self.status = Some(
            if self.show_status_stats {
                "Document statistics on"
            } else {
                "Document statistics off"
            }
            .to_string(),
        );
    }

    pub fn theme(&self) -> &MarkdownTheme {
        &self.theme
    }

    pub fn table_mode(&self) -> bool {
        self.table_mode
    }

    pub fn raw_view(&self) -> bool {
        self.raw_view
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

    pub fn debug_stats(
        &self,
    ) -> (
        DebugTimings,
        view::ViewCacheStats,
        crate::layout::LayoutStats,
    ) {
        let cache = self.view_cache.stats();
        let mut timings = self.timings;
        timings.last_parse_us = cache.last_parse_us;
        (timings, cache, self.layout.stats())
    }

    /// One-line counter summary for the `MARQI_STATS` status-bar segment.
    pub fn stats_line(&self) -> String {
        let ms = |us: u128| us as f64 / 1000.0;
        let (timings, cache, layout) = self.debug_stats();
        format!(
            "lay {:.1} prs {:.1} view {:.1} asm {:.1}ms · blk {} hit {} ren {} · {}KB · rows {}",
            ms(timings.last_layout_us),
            ms(timings.last_parse_us),
            ms(timings.last_view_build_us),
            ms(timings.last_assemble_us),
            cache.blocks_total,
            cache.block_hits,
            cache.block_renders,
            cache.rendered_bytes / 1024,
            layout.rows_live,
        )
    }

    /// Multi-line counter summary printed on exit under `MARQI_STATS`.
    pub fn stats_dump(&self) -> String {
        let ms = |us: u128| us as f64 / 1000.0;
        let (timings, cache, layout) = self.debug_stats();
        let index = self.view_cache.block_stats();
        format!(
            "marqi stats\n\
             \x20 layout: {} rows live · {} rows / {} cells built · last build {:.1}ms\n\
             \x20 parse:  {} blocks · last full partition {:.1}ms · {} windowed / {} full ({} fallbacks)\n\
             \x20 view:   last build {:.1}ms · last assemble {:.1}ms · {} hits · {} renders · {} clears · {}KB cached",
            layout.rows_live,
            layout.rows_built,
            layout.cells_built,
            ms(timings.last_layout_us),
            cache.blocks_total,
            ms(timings.last_parse_us),
            index.windowed_updates,
            index.full_rebuilds,
            index.full_fallbacks,
            ms(timings.last_view_build_us),
            ms(timings.last_assemble_us),
            cache.block_hits,
            cache.block_renders,
            cache.cache_clears,
            cache.rendered_bytes / 1024,
        )
    }

    /// Record the editor area size and refresh the layout/preview/view for it.
    pub fn set_viewport(&mut self, width: usize, height: usize) {
        self.wrap_width = width.max(1);
        self.viewport_height = height;
        // New frame: everything this draw touches is pinned against eviction.
        self.view_cache.begin_frame();
        self.ensure_layout();
        if self.mode.is_read() {
            self.ensure_preview();
        } else {
            self.ensure_view();
        }
    }

    /// Record where the UI drew the content's left edge (margin plus gutter),
    /// so mouse clicks can subtract it.
    pub fn set_left_offset(&mut self, width: usize) {
        self.left_offset = width;
    }

    /// Whether drawing should scroll to keep the cursor visible.
    pub fn follows_cursor(&self) -> bool {
        self.follow_cursor
    }

    // --- input handling ---

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.status = None;
        // Any keystroke ends free (wheel) scrolling: the view snaps back to
        // the cursor so the user is editing what they see.
        self.follow_cursor = true;

        // A bottom-line prompt (e.g. "save as", find) captures all input until
        // it is confirmed or cancelled.
        if self.prompt.is_some() {
            self.handle_prompt_key(key);
            return;
        }

        if self.palette.is_some() {
            self.handle_palette_key(key);
            return;
        }

        if self.outline.is_some() {
            self.handle_outline_key(key);
            return;
        }

        if self.recent_picker.is_some() {
            self.handle_recent_key(key);
            return;
        }

        // The settings popup is modal: it owns all input until accepted (Enter
        // / ^G) or cancelled (Esc), exactly like a prompt.
        if self.menu.is_some() {
            self.handle_menu_key(key);
            return;
        }

        let ctrl = ctrl_like(key.modifiers);

        // Global Ctrl shortcuts. These are used instead of function keys, which
        // many terminals (e.g. macOS Terminal) do not deliver. They are handled
        // before the help/read checks so they work in every preset and mode. In
        // the Emacs preset, ^P and ^B therefore shadow previous-line /
        // backward-char — use the arrow keys for those.
        if ctrl && self.handle_global_ctrl(key) {
            // A global shortcut also ends any pending multi-key chord (Vim
            // `d`/`g`/`y`, Emacs `C-x`); otherwise the stale leader would
            // swallow or reinterpret the next keystroke.
            self.pending = None;
            return;
        }

        // Vim normal mode also opens the settings popup with `?`.
        if self.preset == Preset::Vim && self.mode == Mode::Normal && key.code == KeyCode::Char('?')
        {
            self.open_menu();
            return;
        }

        // ^R toggles the raw (highlighted source) view, except in Vim normal
        // mode where ^R is redo.
        if ctrl
            && key.code == KeyCode::Char('r')
            && !(self.preset == Preset::Vim && self.mode == Mode::Normal)
        {
            self.pending = None;
            self.run_action(Action::ToggleRaw);
            return;
        }

        if self.mode == Mode::Read {
            self.scroll_preview(key);
            return;
        }
        if ctrl && key.code == KeyCode::Char('t') {
            self.pending = None;
            self.run_action(Action::ToggleTable);
            return;
        }
        if self.table_mode && self.handle_table_key(key, ctrl) {
            return;
        }

        match self.preset {
            Preset::Standard => self.handle_standard(key, ctrl),
            Preset::Vim => self.handle_vim(key, ctrl),
            Preset::Nano | Preset::Emacs => self.handle_modeless(key, ctrl),
        }
    }

    /// Dispatch a global Ctrl shortcut. Returns whether the key was handled.
    fn handle_global_ctrl(&mut self, key: KeyEvent) -> bool {
        let action = match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('s' | 'S') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::SaveAs
            }
            KeyCode::Char('s') => Action::Save,
            KeyCode::Char('g') => Action::Settings,
            KeyCode::Char('o' | 'O') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::Outline
            }
            KeyCode::Char('p' | 'P') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::CommandPalette
            }
            KeyCode::Char('p') => Action::TogglePreview,
            KeyCode::Char('l') => Action::CyclePreset,
            KeyCode::Char('b') => Action::ToggleCursor,
            _ => return false,
        };
        self.run_action(action);
        true
    }

    /// Quit, asking for confirmation first when there are unsaved changes.
    fn request_quit(&mut self) {
        if self.buffer.modified() {
            self.open_confirm_quit();
        } else {
            self.should_quit = true;
        }
    }

    fn toggle_preview(&mut self) {
        if self.mode == Mode::Read {
            self.leave_preview();
            return;
        }
        self.save_edit_scroll();
        self.read_cursor = self.cursor.byte;
        self.mode = Mode::Read;
        self.menu = None;
        self.selection_anchor = None;
        self.scroll_y = self.read_scroll_y;
        self.follow_cursor = false;
    }

    /// The mode the editor rests in for the active preset.
    fn resting_mode(&self) -> Mode {
        match self.preset {
            Preset::Standard => Mode::Insert,
            Preset::Vim => Mode::Normal,
            Preset::Nano | Preset::Emacs => Mode::Insert,
        }
    }

    fn cycle_preset(&mut self) {
        self.set_preset(match self.preset {
            Preset::Standard => Preset::Vim,
            Preset::Vim => Preset::Nano,
            Preset::Nano => Preset::Emacs,
            Preset::Emacs => Preset::Standard,
        });
    }

    /// Switch to a specific keybinding preset (used by `^L` cycling and the
    /// settings popup, which steps in both directions).
    fn set_preset(&mut self, preset: Preset) {
        if self.mode == Mode::Read {
            self.leave_preview();
        }
        self.preset = preset;
        self.mode = self.resting_mode();
        self.pending = None;
        self.selection_anchor = None;
        self.table_mode = false;
        self.status = Some(format!("Keybindings: {}", self.preset_label()));
    }

    /// Swap the palette at runtime. Rebuilds the theme and the code
    /// highlighter, then resets every theme-derived cache — the render caches
    /// deliberately assume the theme is immutable (see `view::block_cache_key`),
    /// so a live switch must clear them. Editor-level flags (`heading_glyphs`,
    /// `hard_breaks`) and config colour overrides are preserved across the swap.
    fn apply_theme_runtime(&mut self, name: ThemeName, variant: ThemeVariant) {
        let mut theme = MarkdownTheme::named(name, variant);
        theme.heading_glyphs = self.theme.heading_glyphs;
        theme.hard_breaks = self.theme.hard_breaks;
        theme.apply_overrides(&self.theme_overrides);
        self.theme = theme;
        let syntax = self
            .syntax_override
            .clone()
            .unwrap_or_else(|| self.theme.default_syntax_theme().to_string());
        self.highlighter = CodeHighlighter::new(Some(&syntax));
        // Rendered blocks, heights and the read-mode index all carry the old
        // colours; drop them so the next draw rebuilds with the new palette.
        self.view_cache = ViewCache::default();
        self.view = None;
        self.preview_view = None;
        self.preview_dirty = true;
    }

    fn toggle_cursor_shape(&mut self) {
        self.cursor_shape = match self.cursor_shape {
            CursorShape::Block => CursorShape::Line,
            CursorShape::Line => CursorShape::Block,
        };
        self.status = Some(format!("Cursor: {}", self.cursor_shape.label()));
    }

    /// Toggle the raw view: every line shown as highlighted source (no markers
    /// stripped), still fully editable.
    fn toggle_raw_view(&mut self) {
        if !self.mode.is_read() {
            if self.raw_view {
                self.raw_scroll_y = self.scroll_y;
                self.scroll_y = self.focus_scroll_y;
            } else {
                self.focus_scroll_y = self.scroll_y;
                self.scroll_y = self.raw_scroll_y;
            }
            self.follow_cursor = false;
        }
        self.raw_view = !self.raw_view;
        self.view = None; // force a rebuild with the other builder
        self.status = Some(
            if self.raw_view {
                "Raw view on"
            } else {
                "Raw view off"
            }
            .to_string(),
        );
    }

    /// Read mode: scroll the rendered preview; no text cursor.
    fn scroll_preview(&mut self, key: KeyEvent) {
        let page = self.page_rows();
        let max = self
            .preview_total_rows()
            .saturating_sub(self.viewport_height.max(1));
        let step = |y: usize, delta: isize| (y as isize + delta).clamp(0, max as isize) as usize;
        self.scroll_y = match key.code {
            KeyCode::Up | KeyCode::Char('k') => step(self.scroll_y, -1),
            KeyCode::Down | KeyCode::Char('j') => step(self.scroll_y, 1),
            KeyCode::PageUp => step(self.scroll_y, -(page as isize)),
            KeyCode::PageDown | KeyCode::Char(' ') => step(self.scroll_y, page as isize),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => max,
            KeyCode::Esc | KeyCode::Char('q') => {
                self.leave_preview();
                return;
            }
            _ => self.scroll_y,
        };
    }

    fn save_edit_scroll(&mut self) {
        if self.raw_view {
            self.raw_scroll_y = self.scroll_y;
        } else {
            self.focus_scroll_y = self.scroll_y;
        }
    }

    fn leave_preview(&mut self) {
        self.read_scroll_y = self.scroll_y;
        self.mode = self.resting_mode();
        self.cursor.byte = self.read_cursor.min(self.buffer.len_bytes());
        self.scroll_y = if self.raw_view {
            self.raw_scroll_y
        } else {
            self.focus_scroll_y
        };
        self.follow_cursor = false;
        self.ensure_layout();
        self.cursor.sync_goal(&self.layout);
    }

    /// Apply a motion, updating the selection (`extend` keeps/sets the anchor,
    /// otherwise clears it), breaking the undo run, and refreshing the goal
    /// column for horizontal moves.
    fn do_motion(&mut self, motion: Motion, extend: bool) {
        if extend {
            self.selection_anchor.get_or_insert(self.cursor.byte);
        } else {
            self.selection_anchor = None;
        }
        self.ensure_layout();
        let rope = self.buffer.rope();
        let keep_goal = matches!(
            motion,
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
        );
        match motion {
            Motion::Left => self.cursor.left(rope),
            Motion::Right => self.cursor.right(rope),
            Motion::Up => self.cursor.up(&self.layout),
            Motion::Down => self.cursor.down(&self.layout),
            Motion::LineStart => self.cursor.home(rope),
            Motion::LineEnd => self.cursor.end(rope),
            Motion::WordForward => self.cursor.word_forward(rope),
            Motion::WordBack => self.cursor.word_back(rope),
            Motion::DocStart => self.cursor.doc_start(),
            Motion::DocEnd => self.cursor.doc_end(rope),
            Motion::PageUp => self.cursor.page(&self.layout, self.page_rows(), false),
            Motion::PageDown => self.cursor.page(&self.layout, self.page_rows(), true),
        }
        self.history.break_run();
        if !keep_goal {
            self.cursor.sync_goal(&self.layout);
        }
    }

    // --- Standard ---

    fn handle_standard(&mut self, key: KeyEvent, ctrl: bool) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if ctrl {
            let action = match key.code {
                KeyCode::Char('a') => Some(Action::SelectAll),
                KeyCode::Char('c') => Some(Action::Copy),
                KeyCode::Char('x') => Some(Action::Cut),
                KeyCode::Char('v') => Some(Action::Paste),
                KeyCode::Char('z') if shift => Some(Action::Redo),
                KeyCode::Char('z') => Some(Action::Undo),
                KeyCode::Char('y') => Some(Action::Redo),
                KeyCode::Char('f') => Some(Action::Find),
                KeyCode::Left => {
                    self.do_motion(Motion::WordBack, shift);
                    None
                }
                KeyCode::Right => {
                    self.do_motion(Motion::WordForward, shift);
                    None
                }
                KeyCode::Home => {
                    self.do_motion(Motion::DocStart, shift);
                    None
                }
                KeyCode::End => {
                    self.do_motion(Motion::DocEnd, shift);
                    None
                }
                _ => None,
            };
            if let Some(action) = action {
                self.run_action(action);
            }
            return;
        }

        match key.code {
            KeyCode::Char(c) => self.insert_char_smart(c),
            KeyCode::Enter => self.insert_newline_smart(),
            KeyCode::Tab => self.insert("\t"),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Esc => self.selection_anchor = None,
            _ => self.motion_key(key, shift),
        }
    }

    fn select_all(&mut self) {
        self.selection_anchor = Some(0);
        self.cursor.doc_end(self.buffer.rope());
        self.ensure_layout();
        self.cursor.sync_goal(&self.layout);
        self.history.break_run();
    }

    // --- Vim ---

    fn handle_vim(&mut self, key: KeyEvent, ctrl: bool) {
        match self.mode {
            Mode::Insert => self.vim_insert(key, ctrl),
            Mode::Normal => self.vim_normal(key, ctrl),
            Mode::Visual => self.vim_visual(key),
            Mode::Read => {}
        }
    }

    fn vim_insert(&mut self, key: KeyEvent, ctrl: bool) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Char(c) if !ctrl => self.insert_char_smart(c),
            KeyCode::Enter => self.insert_newline_smart(),
            KeyCode::Tab => self.insert("\t"),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            _ => self.motion_key(key, shift),
        }
    }

    fn vim_normal(&mut self, key: KeyEvent, ctrl: bool) {
        if let Some(op) = self.pending.take() {
            self.vim_operator(op, key);
            return;
        }
        match key.code {
            KeyCode::Char('h') => self.do_motion(Motion::Left, false),
            KeyCode::Char('j') => self.do_motion(Motion::Down, false),
            KeyCode::Char('k') => self.do_motion(Motion::Up, false),
            KeyCode::Char('l') => self.do_motion(Motion::Right, false),
            KeyCode::Char('0') => self.do_motion(Motion::LineStart, false),
            KeyCode::Char('$') => self.do_motion(Motion::LineEnd, false),
            KeyCode::Char('w') => self.do_motion(Motion::WordForward, false),
            KeyCode::Char('b') => self.do_motion(Motion::WordBack, false),
            KeyCode::Char('G') => self.do_motion(Motion::DocEnd, false),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('d') => self.pending = Some('d'),
            KeyCode::Char('y') => self.pending = Some('y'),
            KeyCode::Char('i') => self.mode = Mode::Insert,
            KeyCode::Char('a') => {
                self.do_motion(Motion::Right, false);
                self.mode = Mode::Insert;
            }
            KeyCode::Char('A') => {
                self.do_motion(Motion::LineEnd, false);
                self.mode = Mode::Insert;
            }
            KeyCode::Char('I') => {
                self.do_motion(Motion::LineStart, false);
                self.mode = Mode::Insert;
            }
            KeyCode::Char('o') => self.open_line(true),
            KeyCode::Char('O') => self.open_line(false),
            KeyCode::Char('x') => self.delete_forward(),
            KeyCode::Char('v') => self.enter_visual(),
            KeyCode::Char('u') => self.run_action(Action::Undo),
            KeyCode::Char('r') if ctrl => self.run_action(Action::Redo),
            KeyCode::Char('p') | KeyCode::Char('P') => self.run_action(Action::Paste),
            KeyCode::Char('/') => self.run_action(Action::Find),
            KeyCode::Char('n') => self.find_next(true),
            KeyCode::Char('N') => self.find_next(false),
            _ => self.motion_key(key, false),
        }
    }

    fn vim_operator(&mut self, op: char, key: KeyEvent) {
        match (op, key.code) {
            ('g', KeyCode::Char('g')) => self.do_motion(Motion::DocStart, false),
            ('d', KeyCode::Char('d')) => self.delete_line(),
            ('y', KeyCode::Char('y')) => self.yank_line(),
            _ => {} // unknown sequence: cancelled
        }
    }

    fn vim_visual(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('v') => self.exit_visual(),
            KeyCode::Char('h') => self.do_motion(Motion::Left, true),
            KeyCode::Char('j') => self.do_motion(Motion::Down, true),
            KeyCode::Char('k') => self.do_motion(Motion::Up, true),
            KeyCode::Char('l') => self.do_motion(Motion::Right, true),
            KeyCode::Char('0') => self.do_motion(Motion::LineStart, true),
            KeyCode::Char('$') => self.do_motion(Motion::LineEnd, true),
            KeyCode::Char('w') => self.do_motion(Motion::WordForward, true),
            KeyCode::Char('b') => self.do_motion(Motion::WordBack, true),
            KeyCode::Char('G') => self.do_motion(Motion::DocEnd, true),
            KeyCode::Char('y') => {
                self.copy();
                self.exit_visual();
            }
            KeyCode::Char('d') | KeyCode::Char('x') => {
                self.cut();
                self.mode = Mode::Normal;
            }
            _ => self.motion_key(key, true),
        }
    }

    fn enter_visual(&mut self) {
        self.selection_anchor = Some(self.cursor.byte);
        self.mode = Mode::Visual;
    }

    fn exit_visual(&mut self) {
        self.selection_anchor = None;
        self.mode = Mode::Normal;
    }

    /// Open a new line below (`true`) or above (`false`) and enter insert mode.
    fn open_line(&mut self, below: bool) {
        let nl = self.newline();
        if below {
            self.do_motion(Motion::LineEnd, false);
            self.insert(nl);
        } else {
            self.do_motion(Motion::LineStart, false);
            self.insert(nl);
            self.do_motion(Motion::Up, false);
        }
        self.mode = Mode::Insert;
    }

    fn delete_line(&mut self) {
        let (s, e) = self.current_line_range();
        let text = self.buffer.slice(s, e);
        if text.is_empty() {
            return;
        }
        self.clipboard.set(&text);
        self.history.break_run();
        self.replace_range(s, e, "");
        self.history.break_run();
        self.renumber_ordered_run();
    }

    fn yank_line(&mut self) {
        let (s, e) = self.current_line_range();
        let text = self.buffer.slice(s, e);
        if text.is_empty() {
            return;
        }
        self.clipboard.set(&text);
        self.status = Some("Yanked line".to_string());
    }

    // --- Nano / Emacs (modeless) ---

    fn handle_modeless(&mut self, key: KeyEvent, ctrl: bool) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // An active Emacs mark (C-Space) extends motions until something
        // clears it; without this every motion would drop the region.
        let mark = self.preset == Preset::Emacs && self.selection_anchor.is_some();

        if let Some('x') = self.pending {
            self.pending = None;
            // `C-x C-s` (save) never reaches here: the global ^S handler runs
            // first, saving and clearing the pending leader.
            if ctrl && key.code == KeyCode::Char('c') {
                self.run_action(Action::Quit);
            }
            return;
        }

        if ctrl {
            match (self.preset, key.code) {
                // Universal-ish editing shortcuts.
                (_, KeyCode::Char('z')) => self.run_action(Action::Undo),
                // ^F find, plus ^W — nano's native "Where Is" (Emacs keeps
                // C-f as forward-char and gets find on M-s).
                (Preset::Nano, KeyCode::Char('f')) => self.run_action(Action::Find),
                (Preset::Nano, KeyCode::Char('w')) => self.run_action(Action::Find),
                (Preset::Nano, KeyCode::Char('k')) => self.run_action(Action::Cut),
                (Preset::Nano, KeyCode::Char('u')) => self.run_action(Action::Paste),
                (Preset::Nano, KeyCode::Char('o')) => self.run_action(Action::Save),
                (Preset::Emacs, KeyCode::Char('x')) => self.pending = Some('x'),
                (Preset::Emacs, KeyCode::Char('a')) => self.do_motion(Motion::LineStart, mark),
                (Preset::Emacs, KeyCode::Char('e')) => self.do_motion(Motion::LineEnd, mark),
                (Preset::Emacs, KeyCode::Char('f')) => self.do_motion(Motion::Right, mark),
                // C-b and C-p are global toggles (cursor shape / preview), so
                // they never arrive here — backward-char and previous-line are
                // available on the arrow keys.
                (Preset::Emacs, KeyCode::Char('n')) => self.do_motion(Motion::Down, mark),
                (Preset::Emacs, KeyCode::Char('w')) => self.run_action(Action::Cut),
                (Preset::Emacs, KeyCode::Char('y')) => self.run_action(Action::Paste),
                (Preset::Emacs, KeyCode::Char(' ')) => {
                    self.selection_anchor = Some(self.cursor.byte);
                    self.status = Some("Mark set".to_string());
                }
                _ => {}
            }
            return;
        }

        if alt && self.preset == Preset::Emacs {
            match key.code {
                KeyCode::Char('w') => {
                    self.run_action(Action::Copy);
                    // Emacs deactivates the region after a kill-ring save.
                    self.selection_anchor = None;
                    return;
                }
                KeyCode::Char('f') => return self.do_motion(Motion::WordForward, mark),
                KeyCode::Char('b') => return self.do_motion(Motion::WordBack, mark),
                // Emacs' search prefix; C-s itself is the global save.
                KeyCode::Char('s') => return self.run_action(Action::Find),
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char(c) => self.insert_char_smart(c),
            KeyCode::Enter => self.insert_newline_smart(),
            KeyCode::Tab => self.insert("\t"),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Esc => self.selection_anchor = None,
            _ => self.motion_key(key, shift || mark),
        }
    }

    // --- mouse ---

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        match event.kind {
            MouseEventKind::ScrollUp => self.scroll_wheel(-3),
            MouseEventKind::ScrollDown => self.scroll_wheel(3),
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(event.column, event.row),
            MouseEventKind::Drag(MouseButton::Left) => self.mouse_drag(event.column, event.row),
            _ => {}
        }
    }

    /// Wheel scrolling moves the viewport freely; the cursor stays put and the
    /// view stops following it until the next keypress or click.
    fn scroll_wheel(&mut self, delta: isize) {
        if self.palette.is_some() || self.outline.is_some() || self.recent_picker.is_some() {
            return;
        }
        if let Some(menu) = self.menu.as_mut() {
            menu.guide_scroll = menu.guide_scroll.saturating_add_signed(delta);
            return;
        }
        let max = if self.mode.is_read() {
            self.preview_total_rows()
        } else {
            self.ensure_view();
            self.follow_cursor = false;
            self.view().total_rows()
        }
        .saturating_sub(self.viewport_height.max(1));
        self.scroll_y = self.scroll_y.saturating_add_signed(delta).min(max);
    }

    fn mouse_down(&mut self, x: u16, y: u16) {
        if let Some(line) = self.task_line_at_click(x, y) {
            self.cursor.byte = self.buffer.rope().line_to_byte(line);
            self.run_action(Action::ToggleTask);
            self.after_mouse_move();
            return;
        }
        let Some(byte) = self.byte_at_screen(x, y) else {
            return;
        };
        if self.mode == Mode::Visual {
            self.exit_visual();
        }
        self.selection_anchor = None;
        self.mouse_press_byte = Some(byte);
        self.cursor.byte = byte;
        self.after_mouse_move();
    }

    fn task_line_at_click(&mut self, x: u16, y: u16) -> Option<usize> {
        if self.menu.is_some()
            || self.palette.is_some()
            || self.outline.is_some()
            || self.recent_picker.is_some()
            || self.mode.is_read()
            || y as usize >= self.viewport_height
        {
            return None;
        }
        let assembled = self.visible_rows();
        let row = assembled.lines.get(y as usize)?;
        let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
        let glyph = text.find(['■', '□'])?;
        let glyph_col = UnicodeWidthStr::width(&text[..glyph]);
        let click_col = (x as usize).saturating_sub(self.left_offset);
        if click_col != glyph_col {
            return None;
        }
        assembled
            .numbers
            .get(y as usize)
            .copied()
            .flatten()
            .map(|line| line - 1)
    }

    fn mouse_drag(&mut self, x: u16, y: u16) {
        let Some(press) = self.mouse_press_byte else {
            return;
        };
        let Some(byte) = self.byte_at_screen(x, y) else {
            return;
        };
        if byte != press {
            self.selection_anchor = Some(press);
            // Dragging in the Vim preset behaves like entering Visual mode.
            if self.preset == Preset::Vim && self.mode == Mode::Normal {
                self.mode = Mode::Visual;
            }
        }
        self.cursor.byte = byte;
        self.after_mouse_move();
    }

    fn after_mouse_move(&mut self) {
        self.follow_cursor = true;
        self.ensure_layout();
        self.cursor.sync_goal(&self.layout);
        self.history.break_run();
    }

    /// Map a screen position to a buffer byte. Raw rows (the active block,
    /// gaps, and the whole raw view) map exactly; preview rows map to the
    /// clicked column within their source line, which is close enough since
    /// the click immediately opens that line as raw.
    fn byte_at_screen(&mut self, x: u16, y: u16) -> Option<usize> {
        if self.menu.is_some()
            || self.palette.is_some()
            || self.outline.is_some()
            || self.recent_picker.is_some()
            || self.mode.is_read()
            || y as usize >= self.viewport_height
        {
            return None;
        }
        self.ensure_layout();
        self.ensure_view();
        let view = self.view.as_ref().expect("view built before mouse mapping");
        let row = (self.scroll_y + y as usize).min(view.total_rows().saturating_sub(1));
        let col = (x as usize).saturating_sub(self.left_offset) as u16;

        // Rows inside the active raw run map 1:1 onto layout rows.
        let active_row_count = self
            .layout
            .first_row_of_line(view.active_lines.1 + 1)
            .saturating_sub(view.active_first_row);
        if row >= view.active_screen_row && row < view.active_screen_row + active_row_count {
            let layout_row = view.active_first_row + (row - view.active_screen_row);
            return Some(self.layout.pos_to_byte(layout_row, col));
        }

        // Elsewhere the row resolves within its segment: raw runs map exactly
        // onto layout rows; preview rows carry a source line number where the
        // renderer knows one exactly (list items, table grid rows, headings),
        // and chrome rows (table borders, wrapped continuations) estimate from
        // the nearest label in the same block — close enough, since the click
        // immediately opens that block as raw source where mapping is exact.
        let target = view::row_target(
            self.view.as_ref().expect("view built before mouse mapping"),
            &mut self.view_cache,
            self.buffer.rope(),
            self.wrap_width,
            &self.theme,
            &self.highlighter,
            row,
        )?;
        let layout_row = match target {
            view::RowTarget::LayoutRow(layout_row) => layout_row,
            view::RowTarget::SourceLine(line0) => {
                let total = self.buffer.rope().len_lines();
                self.layout
                    .first_row_of_line(line0.min(total.saturating_sub(1)))
            }
        };
        Some(self.layout.pos_to_byte(layout_row, col))
    }

    // --- auto-save ---

    /// Whether the idle loop should tick (poll with a timeout) for auto-save.
    pub fn wants_tick(&self) -> bool {
        self.buffer.modified()
            && self.prompt.is_none()
            && (self.recovery_written_version != self.version
                || (self.auto_save && self.buffer.has_path()))
    }

    /// Idle callback from the event loop: auto-save once the buffer has been
    /// quiet for [`AUTO_SAVE_DELAY`]. Only named buffers save automatically —
    /// popping a "save as" prompt mid-thought would be hostile.
    pub fn tick(&mut self) {
        if !self.wants_tick() {
            return;
        }
        let now = Instant::now();
        if self.auto_save_retry_at.is_some_and(|retry| retry > now) {
            return;
        }
        let idle = self
            .last_edit
            .is_none_or(|at| at.elapsed() >= AUTO_SAVE_DELAY);
        if !idle {
            return;
        }
        if self.recovery_written_version != self.version {
            match self.buffer.write_recovery() {
                Ok(()) => self.recovery_written_version = self.version,
                Err(error) => {
                    self.auto_save_retry_at = Some(now + AUTO_SAVE_RETRY_DELAY);
                    self.status = Some(format!("Recovery failed: {error}"));
                    return;
                }
            }
        }
        if !self.auto_save || !self.buffer.has_path() {
            return;
        }
        match self.buffer.has_external_change() {
            Ok(true) => {
                self.auto_save_retry_at = Some(now + AUTO_SAVE_RETRY_DELAY);
                self.status = Some("Auto-save paused: file changed on disk".to_string());
                return;
            }
            Err(error) => {
                self.auto_save_retry_at = Some(now + AUTO_SAVE_RETRY_DELAY);
                self.status = Some(format!("Auto-save failed: {error}"));
                return;
            }
            Ok(false) => {}
        }
        match self.buffer.save() {
            Ok(()) => {
                self.history.mark_saved();
                self.auto_save_retry_at = None;
                self.status = Some("Auto-saved".to_string());
            }
            Err(error) => {
                self.auto_save_retry_at = Some(now + AUTO_SAVE_RETRY_DELAY);
                self.status = Some(format!("Auto-save failed: {error}"));
            }
        }
    }

    /// Handle the navigation keys common to every mode (arrows, Home/End,
    /// PageUp/Dn). `extend` controls whether the selection grows.
    fn motion_key(&mut self, key: KeyEvent, extend: bool) {
        let motion = match key.code {
            KeyCode::Left => Motion::Left,
            KeyCode::Right => Motion::Right,
            KeyCode::Up => Motion::Up,
            KeyCode::Down => Motion::Down,
            KeyCode::Home => Motion::LineStart,
            KeyCode::End => Motion::LineEnd,
            KeyCode::PageUp => Motion::PageUp,
            KeyCode::PageDown => Motion::PageDown,
            _ => return,
        };
        self.do_motion(motion, extend);
    }

    fn line_without_eol(&self, line: usize) -> String {
        self.buffer
            .rope()
            .line(line)
            .to_string()
            .trim_end_matches(['\n', '\r'])
            .to_string()
    }

    fn line_has_eol(&self, line: usize) -> bool {
        self.buffer
            .rope()
            .line(line)
            .to_string()
            .ends_with(['\n', '\r'])
    }

    fn line_range(&self, line: usize) -> (usize, usize) {
        let rope = self.buffer.rope();
        let start = rope.line_to_byte(line);
        let end = if line + 1 < rope.len_lines() {
            rope.line_to_byte(line + 1)
        } else {
            rope.len_bytes()
        };
        (start, end)
    }

    // --- editing primitives ---

    /// Replace byte range `[start, end)` with `text`, recording the reversible
    /// edit and leaving the cursor after the inserted text.
    fn replace_range(&mut self, start: usize, end: usize, text: &str) {
        self.replace_range_with_cursor(start, end, text, None);
    }

    /// Like [`Self::replace_range`], but lets smart editing place the cursor
    /// inside the inserted text while recording that position for redo.
    fn replace_range_with_cursor(
        &mut self,
        start: usize,
        end: usize,
        text: &str,
        cursor_after: Option<usize>,
    ) {
        // A no-op "edit" must not reach the history: recording it would clear
        // the redo stack (destroying real redo state) for nothing.
        if end <= start && text.is_empty() {
            return;
        }
        let impact = self.edit_impact(start, end, text);
        let removed = self.buffer.slice(start, end);
        let cursor_before = self.cursor.byte;
        if end > start {
            self.buffer.remove(start, end);
        }
        let inserted_end = if text.is_empty() {
            start
        } else {
            self.buffer.insert(start, text)
        };
        self.cursor.byte = cursor_after
            .unwrap_or(inserted_end)
            .min(self.buffer.len_bytes());
        self.history.record(Edit {
            start,
            removed,
            inserted: text.to_string(),
            cursor_before,
            cursor_after: self.cursor.byte,
        });
        self.selection_anchor = None;
        self.mark_edited(impact);
        self.sync_modified();
    }

    /// The line ending to insert for Enter and friends, matching the file.
    fn newline(&self) -> &'static str {
        self.buffer.newline()
    }

    /// Keep the buffer's modified flag in step with the undo history, so
    /// undoing back to the last-saved state reads as unmodified again.
    fn sync_modified(&mut self) {
        self.buffer.set_modified(!self.history.at_saved_state());
    }

    /// Insert `text`, replacing the selection if there is one.
    fn insert(&mut self, text: &str) {
        match self.selection_range() {
            Some((s, e)) => self.replace_range(s, e, text),
            None => {
                let at = self.cursor.byte;
                self.replace_range(at, at, text);
            }
        }
    }

    fn backspace(&mut self) {
        let (s, e) = if let Some(range) = self.selection_range() {
            range
        } else if self.cursor.byte > 0 {
            (
                prev_grapheme(self.buffer.rope(), self.cursor.byte),
                self.cursor.byte,
            )
        } else {
            return;
        };
        // A deletion that crosses a line boundary may add/remove a list item.
        let structural = self.buffer.slice(s, e).contains('\n');
        self.replace_range(s, e, "");
        if structural {
            self.renumber_ordered_run();
        }
    }

    fn delete_forward(&mut self) {
        let (s, e) = if let Some(range) = self.selection_range() {
            range
        } else if self.cursor.byte < self.buffer.len_bytes() {
            (
                self.cursor.byte,
                next_grapheme(self.buffer.rope(), self.cursor.byte),
            )
        } else {
            return;
        };
        let structural = self.buffer.slice(s, e).contains('\n');
        self.replace_range(s, e, "");
        if structural {
            self.renumber_ordered_run();
        }
    }

    // --- undo / redo ---

    fn undo(&mut self) {
        match self.history.undo() {
            Some(e) => {
                let impact = self.edit_impact(e.start, e.start + e.inserted.len(), &e.removed);
                self.buffer.remove(e.start, e.start + e.inserted.len());
                if !e.removed.is_empty() {
                    self.buffer.insert(e.start, &e.removed);
                }
                self.cursor.byte = e.cursor_before;
                self.selection_anchor = None;
                self.mark_edited(impact);
                self.sync_modified();
            }
            None => self.status = Some("Nothing to undo".to_string()),
        }
    }

    fn redo(&mut self) {
        match self.history.redo() {
            Some(e) => {
                let impact = self.edit_impact(e.start, e.start + e.removed.len(), &e.inserted);
                self.buffer.remove(e.start, e.start + e.removed.len());
                if !e.inserted.is_empty() {
                    self.buffer.insert(e.start, &e.inserted);
                }
                self.cursor.byte = e.cursor_after;
                self.selection_anchor = None;
                self.mark_edited(impact);
                self.sync_modified();
            }
            None => self.status = Some("Nothing to redo".to_string()),
        }
    }

    // --- selection & clipboard ---

    /// The selection as an ordered byte range, or `None` when empty.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        let cursor = self.cursor.byte;
        (anchor != cursor).then_some((anchor.min(cursor), anchor.max(cursor)))
    }

    /// Byte range of the cursor's logical line, including its trailing newline.
    fn current_line_range(&self) -> (usize, usize) {
        let line = self.buffer.rope().byte_to_line(self.cursor.byte);
        self.line_range(line)
    }

    /// Copy the selection (or the current line) as source markdown.
    fn copy(&mut self) {
        let (s, e) = self
            .selection_range()
            .unwrap_or_else(|| self.current_line_range());
        let text = self.buffer.slice(s, e);
        self.clipboard.set(&text);
        self.status = Some("Copied".to_string());
    }

    /// Cut the selection (or the current line) as source markdown.
    fn cut(&mut self) {
        let (s, e) = self
            .selection_range()
            .unwrap_or_else(|| self.current_line_range());
        let text = self.buffer.slice(s, e);
        if text.is_empty() {
            return;
        }
        let structural = text.contains('\n');
        self.clipboard.set(&text);
        self.history.break_run();
        self.replace_range(s, e, "");
        self.history.break_run();
        self.status = Some("Cut".to_string());
        if structural {
            self.renumber_ordered_run();
        }
    }

    /// Paste clipboard text, replacing the selection if there is one.
    fn paste(&mut self) {
        let text = self.clipboard.get();
        if text.is_empty() {
            return;
        }
        self.history.break_run();
        self.insert(&text);
        self.history.break_run();
        self.status = Some("Pasted".to_string());
    }

    fn edit_impact(&self, start: usize, end: usize, inserted: &str) -> EditImpact {
        let rope = self.buffer.rope();
        let len = rope.len_bytes();
        let start = start.min(len);
        let end = end.min(len);
        let start_line = rope.byte_to_line(start);
        let end_line = rope.byte_to_line(end);
        EditImpact {
            start_line,
            old_line_count: end_line.saturating_sub(start_line) + 1,
            start_byte: start,
            inserted_len: inserted.len(),
            removed_len: end - start,
        }
    }

    /// Invalidate or incrementally refresh the derived views after a buffer mutation.
    fn mark_edited(&mut self, impact: EditImpact) {
        self.version += 1;
        self.last_edit = Some(Instant::now());
        self.auto_save_retry_at = None;
        // The replaced span's new line count, from the post-mutation rope so
        // every line-break form ropey recognizes is counted.
        let rope = self.buffer.rope();
        let inserted_end = (impact.start_byte + impact.inserted_len).min(rope.len_bytes());
        let new_line_count = rope
            .byte_to_line(inserted_end)
            .saturating_sub(impact.start_line)
            + 1;
        // Patch the layout incrementally only when it is current and built for
        // this width; otherwise mark it dirty so `ensure_layout` rebuilds it.
        if !self.layout_dirty && self.layout_width == self.wrap_width {
            self.layout.update_after_edit(
                self.buffer.rope(),
                self.wrap_width,
                self.tab_width,
                impact.start_line,
                impact.old_line_count,
                new_line_count,
            );
        } else {
            self.layout_dirty = true;
        }
        let update = self.line_index.apply_edit(
            self.buffer.rope(),
            impact.start_line,
            impact.old_line_count,
            new_line_count,
        );
        self.view_cache.apply_edit(
            self.buffer.rope(),
            crate::block_index::EditSpan {
                start_line: impact.start_line,
                old_line_count: impact.old_line_count,
                new_line_count,
                byte_delta: impact.inserted_len as isize - impact.removed_len as isize,
            },
            &update,
            &self.line_index,
            self.version,
        );
        self.preview_dirty = true;
    }

    fn save(&mut self) {
        if self.buffer.has_path() {
            match self.buffer.has_external_change() {
                Ok(true) => self.open_external_change(),
                Ok(false) => self.write_buffer(),
                Err(error) => self.status = Some(format!("Error: {error}")),
            }
        } else {
            self.open_save_as();
        }
    }

    /// Write the buffer to its known path and report the result.
    fn write_buffer(&mut self) {
        self.status = Some(match self.buffer.save() {
            Ok(()) => {
                self.history.mark_saved();
                self.auto_save_retry_at = None;
                "Saved".to_string()
            }
            Err(e) => format!("Error: {e}"),
        });
    }

    fn write_buffer_force(&mut self) {
        self.status = Some(match self.buffer.save_force() {
            Ok(()) => {
                self.history.mark_saved();
                self.auto_save_retry_at = None;
                "Saved".to_string()
            }
            Err(error) => format!("Error: {error}"),
        });
    }

    fn reload_buffer(&mut self) -> anyhow::Result<()> {
        self.buffer.reload()?;
        self.history = History::new();
        self.reset_after_buffer_change();
        Ok(())
    }

    fn reset_after_buffer_change(&mut self) {
        self.cursor.byte = self.cursor.byte.min(self.buffer.len_bytes());
        self.selection_anchor = None;
        self.layout = Layout::build(self.buffer.rope(), self.wrap_width, self.tab_width);
        self.layout_width = self.wrap_width;
        self.layout_dirty = false;
        self.line_index = LineIndex::build(self.buffer.rope());
        self.version += 1;
        self.view = None;
        self.view_cache = ViewCache::default();
        self.preview_view = None;
        self.preview_dirty = true;
        self.scroll_y = 0;
        self.auto_save_retry_at = None;
        self.recovery_written_version = self.version;
    }

    pub fn offer_recovery(&mut self) {
        match self.buffer.load_recovery() {
            Ok(Some(content)) => {
                self.recovery_content = Some(content);
                self.prompt = Some(prompt::Prompt::Recovery { error: None });
            }
            Ok(None) => {}
            Err(error) => self.status = Some(format!("Recovery unavailable: {error}")),
        }
    }

    fn restore_recovery(&mut self) -> anyhow::Result<()> {
        let content = self
            .recovery_content
            .take()
            .ok_or_else(|| anyhow::anyhow!("recovery content is unavailable"))?;
        self.buffer.restore_recovery(&content);
        self.history = History::new();
        self.history.mark_unsaved();
        self.reset_after_buffer_change();
        self.last_edit = Some(Instant::now());
        Ok(())
    }

    // --- layout & scrolling ---

    fn ensure_layout(&mut self) {
        if self.layout_dirty || self.layout_width != self.wrap_width {
            let started = Instant::now();
            self.layout = Layout::build(self.buffer.rope(), self.wrap_width, self.tab_width);
            self.timings.last_layout_us = started.elapsed().as_micros();
            self.layout_width = self.wrap_width;
            self.layout_dirty = false;
        }
    }

    /// Rebuild the read-mode row index if the content or width changed.
    fn ensure_preview(&mut self) {
        if self.preview_view.is_none()
            || self.preview_dirty
            || self.preview_width != self.wrap_width
        {
            let started = Instant::now();
            self.preview_view = Some(view::build_preview_index(
                &mut self.view_cache,
                self.buffer.rope(),
                self.wrap_width,
                &self.theme,
                &self.highlighter,
                self.version,
            ));
            self.timings.last_view_build_us = started.elapsed().as_micros();
            self.preview_width = self.wrap_width;
            self.preview_dirty = false;
        }
    }

    /// Total rendered rows of the read-mode preview.
    fn preview_total_rows(&mut self) -> usize {
        self.ensure_preview();
        self.preview_view
            .as_ref()
            .expect("preview built above")
            .total_rows()
    }

    /// Rebuild the hybrid row index if the content/width changed, or the
    /// cursor left the active region (which changes what renders raw).
    /// Selection and cursor-line styling are applied at assembly time, so
    /// neither invalidates the structure.
    fn ensure_view(&mut self) {
        if self.raw_view {
            return self.ensure_raw_view();
        }
        let cursor_line = self.buffer.rope().byte_to_line(self.cursor.byte);
        let still_valid = self.view.as_ref().is_some_and(|v| {
            self.view_version == self.version
                && self.view_width == self.wrap_width
                && (v.active_lines.0..=v.active_lines.1).contains(&cursor_line)
        });
        if !still_valid {
            let started = Instant::now();
            self.view = Some(view::build_index(
                &mut self.view_cache,
                self.buffer.rope(),
                &self.layout,
                self.cursor.byte,
                self.wrap_width,
                &self.theme,
                &self.highlighter,
                self.version,
            ));
            self.timings.last_view_build_us = started.elapsed().as_micros();
            self.view_version = self.version;
            self.view_width = self.wrap_width;
        }
    }

    /// Rebuild the raw (highlighted source) row index if the content or width
    /// changed. Cursor and selection are assembly inputs.
    fn ensure_raw_view(&mut self) {
        let still_valid = self.view.is_some()
            && self.view_version == self.version
            && self.view_width == self.wrap_width;
        if !still_valid {
            let started = Instant::now();
            self.view = Some(view::build_raw_index(self.buffer.rope(), &self.layout));
            self.timings.last_view_build_us = started.elapsed().as_micros();
            self.view_version = self.version;
            self.view_width = self.wrap_width;
        }
    }

    /// Render the rows currently in the viewport (plus gutter labels). The
    /// only place rendered rows are produced — cost is O(viewport), not
    /// O(document).
    pub fn visible_rows(&mut self) -> view::Assembled {
        let started = Instant::now();
        let out = if self.mode.is_read() {
            self.ensure_preview();
            let lines = view::assemble_preview(
                self.preview_view.as_ref().expect("preview built above"),
                &mut self.view_cache,
                self.buffer.rope(),
                self.wrap_width,
                &self.theme,
                &self.highlighter,
                self.scroll_y,
                self.viewport_height.max(1),
            );
            let numbers = vec![None; lines.len()];
            view::Assembled { lines, numbers }
        } else {
            self.ensure_layout();
            self.ensure_view();
            self.assemble_rows(self.scroll_y, self.viewport_height.max(1))
        };
        self.timings.last_assemble_us = started.elapsed().as_micros();
        out
    }

    /// Assemble every rendered row — the old full-document view, for tests.
    #[cfg(test)]
    pub fn all_rows(&mut self) -> view::Assembled {
        self.ensure_layout();
        self.ensure_view();
        let total = self.view().total_rows();
        self.assemble_rows(0, total)
    }

    fn assemble_rows(&mut self, start: usize, count: usize) -> view::Assembled {
        let cursor_line = self.buffer.rope().byte_to_line(self.cursor.byte);
        let selection = self.selection_range();
        view::assemble(
            self.view.as_ref().expect("view built before assembly"),
            &mut self.view_cache,
            self.buffer.rope(),
            &self.layout,
            &self.line_index,
            cursor_line,
            selection,
            self.wrap_width,
            &self.theme,
            &self.highlighter,
            start,
            count,
        )
    }

    /// Scroll the hybrid view so the cursor stays visible with a scrolloff
    /// margin. (Read mode scrolls itself in `scroll_preview`.)
    pub fn scroll_to_cursor(&mut self) {
        self.ensure_view();
        let (_, row) = self.cursor_screen();
        let total = self.view().total_rows();
        let height = self.viewport_height.max(1);
        let margin = self.scrolloff.min(height.saturating_sub(1) / 2);

        if row < self.scroll_y + margin {
            self.scroll_y = row.saturating_sub(margin);
        }
        if row + margin >= self.scroll_y + height {
            self.scroll_y = row + margin + 1 - height;
        }
        let max_scroll = total.saturating_sub(height);
        if self.scroll_y > max_scroll {
            self.scroll_y = max_scroll;
        }
    }

    /// Clamp the free-scrolled viewport to the content (used while wheel
    /// scrolling has detached the view from the cursor).
    pub fn clamp_scroll(&mut self) {
        self.ensure_view();
        let max = self
            .view()
            .total_rows()
            .saturating_sub(self.viewport_height.max(1));
        self.scroll_y = self.scroll_y.min(max);
    }

    fn page_rows(&self) -> usize {
        self.viewport_height.max(2) - 1
    }

    /// `(1-based line, 1-based display column)` for the status bar.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let line = self.buffer.rope().byte_to_line(self.cursor.byte) + 1;
        let (_, col) = self.layout.byte_to_pos(self.cursor.byte);
        (line, col as usize + 1)
    }
}
