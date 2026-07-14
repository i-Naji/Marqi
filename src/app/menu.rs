//! The settings popup: an interactive overlay for switching the theme,
//! light/dark appearance, and keybinding preset, with a platform-aware
//! keybinding guide filtered to the active preset (rendered in [`crate::ui`]).
//!
//! Modelled on the [`super::prompt`] pattern: a single `Option<Menu>` on `App`
//! that captures all input while open. Theme and appearance changes apply
//! **live** behind the popup; `Esc` reverts to the snapshot taken when it
//! opened, `Enter` / `^G` accept and close.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, LineNumbers, Preset, ctrl_like};
use crate::markdown::{ThemeName, ThemeVariant};

const ROW_KEYBINDINGS: usize = 0;
const ROW_THEME: usize = 1;
const ROW_APPEARANCE: usize = 2;
const ROW_LINE_NUMBERS: usize = 3;
const ROW_SAVE_DEFAULTS: usize = 4;
const LAST_ROW: usize = ROW_SAVE_DEFAULTS;

/// Preset cycle order for the keybindings row.
const PRESETS: [Preset; 4] = [Preset::Standard, Preset::Vim, Preset::Nano, Preset::Emacs];
/// Line-number cycle order.
const LINE_NUMBERS: [LineNumbers; 3] = [
    LineNumbers::Off,
    LineNumbers::Absolute,
    LineNumbers::Relative,
];

pub struct Menu {
    /// Focused setting row (0 = keybindings, 1 = theme, 2 = appearance,
    /// 3 = line numbers).
    pub focus: usize,
    /// Scroll offset into the keybinding guide below the settings rows.
    pub guide_scroll: usize,
    // What to restore on Esc — captured when the popup opened.
    orig_preset: Preset,
    orig_theme: ThemeName,
    orig_variant: ThemeVariant,
    orig_line_numbers: LineNumbers,
}

impl App {
    /// Open the settings popup, snapshotting the current selections so Esc can
    /// revert any live theme/preset changes.
    pub(super) fn open_menu(&mut self) {
        self.menu = Some(Menu {
            focus: 0,
            guide_scroll: 0,
            orig_preset: self.preset,
            orig_theme: self.theme.name,
            orig_variant: self.theme.variant,
            orig_line_numbers: self.line_numbers,
        });
    }

    pub(super) fn handle_menu_key(&mut self, key: KeyEvent) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let focus = menu.focus;

        if key.code == KeyCode::Enter && focus == ROW_SAVE_DEFAULTS {
            self.save_menu_defaults();
            return;
        }
        if key.code == KeyCode::Enter
            || (ctrl_like(key.modifiers) && key.code == KeyCode::Char('g'))
        {
            self.menu = None;
            return;
        }

        match key.code {
            KeyCode::Esc => {
                let (preset, theme, variant, line_numbers) = {
                    let m = self.menu.as_ref().unwrap();
                    (
                        m.orig_preset,
                        m.orig_theme,
                        m.orig_variant,
                        m.orig_line_numbers,
                    )
                };
                self.apply_theme_runtime(theme, variant);
                self.set_preset(preset);
                self.line_numbers = line_numbers;
                self.status = None;
                self.menu = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.menu.as_mut().unwrap().focus = focus.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.menu.as_mut().unwrap().focus = (focus + 1).min(LAST_ROW);
            }
            KeyCode::Left | KeyCode::Char('h') => self.menu_adjust(focus, false),
            KeyCode::Right | KeyCode::Char('l') => self.menu_adjust(focus, true),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                let m = self.menu.as_mut().unwrap();
                m.guide_scroll = m.guide_scroll.saturating_add(4);
            }
            KeyCode::PageUp => {
                let m = self.menu.as_mut().unwrap();
                m.guide_scroll = m.guide_scroll.saturating_sub(4);
            }
            _ => {}
        }
    }

    /// Step the focused row's value one place (`forward` = right / next),
    /// applying the change live.
    fn menu_adjust(&mut self, focus: usize, forward: bool) {
        match focus {
            ROW_KEYBINDINGS => {
                let i = PRESETS.iter().position(|p| *p == self.preset).unwrap_or(0);
                self.set_preset(PRESETS[step(i, PRESETS.len(), forward)]);
            }
            ROW_THEME => {
                let all = ThemeName::ALL;
                let i = all.iter().position(|n| *n == self.theme.name).unwrap_or(0);
                self.apply_theme_runtime(all[step(i, all.len(), forward)], self.theme.variant);
            }
            ROW_APPEARANCE => {
                let variant = match self.theme.variant {
                    ThemeVariant::Dark => ThemeVariant::Light,
                    ThemeVariant::Light => ThemeVariant::Dark,
                };
                self.apply_theme_runtime(self.theme.name, variant);
            }
            ROW_LINE_NUMBERS => {
                let i = LINE_NUMBERS
                    .iter()
                    .position(|n| *n == self.line_numbers)
                    .unwrap_or(0);
                // Pure rendering state — the gutter re-reads it next draw, no
                // cache reset needed.
                self.line_numbers = LINE_NUMBERS[step(i, LINE_NUMBERS.len(), forward)];
            }
            _ => {}
        }
    }

    fn save_menu_defaults(&mut self) {
        let Some(path) = self.config_path.as_deref() else {
            self.status = Some("Config directory is unavailable".to_string());
            self.menu = None;
            return;
        };
        let keybindings = self.preset_label().to_ascii_lowercase();
        let line_numbers = match self.line_numbers {
            LineNumbers::Off => "off",
            LineNumbers::Absolute => "absolute",
            LineNumbers::Relative => "relative",
        };
        let variant = self.theme.variant.label().to_ascii_lowercase();
        self.status = Some(
            match crate::config::save_runtime_settings(
                path,
                &keybindings,
                line_numbers,
                self.theme.name.config_name(),
                &variant,
            ) {
                Ok(()) => "Settings saved".to_string(),
                Err(error) => format!("Settings not saved: {error}"),
            },
        );
        self.menu = None;
    }
}

/// Wrapping index step over `len` items.
fn step(i: usize, len: usize, forward: bool) -> usize {
    if forward {
        (i + 1) % len
    } else {
        (i + len - 1) % len
    }
}
