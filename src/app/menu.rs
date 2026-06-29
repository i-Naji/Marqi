//! The settings popup: an interactive overlay for switching the theme,
//! light/dark appearance, and keybinding preset, with a platform-aware
//! keybinding guide filtered to the active preset (rendered in [`crate::ui`]).
//!
//! Modelled on the [`super::prompt`] pattern: a single `Option<Menu>` on `App`
//! that captures all input while open. Theme and appearance changes apply
//! **live** behind the popup; `Esc` reverts to the snapshot taken when it
//! opened, `Enter` / `^G` accept and close.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Preset, ctrl_like};
use crate::markdown::{ThemeName, ThemeVariant};

const ROW_KEYBINDINGS: usize = 0;
const ROW_THEME: usize = 1;
const ROW_APPEARANCE: usize = 2;
const LAST_ROW: usize = ROW_APPEARANCE;

/// Preset cycle order for the keybindings row.
const PRESETS: [Preset; 4] = [Preset::Standard, Preset::Vim, Preset::Nano, Preset::Emacs];

pub struct Menu {
    /// Focused setting row (0 = keybindings, 1 = theme, 2 = appearance).
    pub focus: usize,
    /// Scroll offset into the keybinding guide below the settings rows.
    pub guide_scroll: usize,
    // What to restore on Esc — captured when the popup opened.
    orig_preset: Preset,
    orig_theme: ThemeName,
    orig_variant: ThemeVariant,
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
        });
    }

    pub(super) fn handle_menu_key(&mut self, key: KeyEvent) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let focus = menu.focus;

        // Accept (keep the live changes): Enter or ^G.
        if key.code == KeyCode::Enter
            || (ctrl_like(key.modifiers) && key.code == KeyCode::Char('g'))
        {
            self.menu = None;
            return;
        }

        match key.code {
            KeyCode::Esc => {
                let (preset, theme, variant) = {
                    let m = self.menu.as_ref().unwrap();
                    (m.orig_preset, m.orig_theme, m.orig_variant)
                };
                self.apply_theme_runtime(theme, variant);
                self.set_preset(preset);
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
            _ => {}
        }
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
