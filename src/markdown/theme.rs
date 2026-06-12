//! Visual theme for rendered markdown: a ratatui [`Style`] per element.
//!
//! These hard-coded defaults (a dark and a light variant) can be overridden per
//! element from `[theme.markdown]` config via [`MarkdownTheme::apply_overrides`].

use std::collections::HashMap;
use std::env;

use ratatui::style::{Color, Modifier, Style};

use crate::color::{parse_hex, rgb};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeVariant {
    Dark,
    Light,
}

pub struct MarkdownTheme {
    pub variant: ThemeVariant,
    pub background: Color,
    pub active_line: Color,
    pub gutter: Style,
    pub gutter_current: Style,
    pub status: Style,
    pub help_background: Color,
    pub text: Style,
    pub headings: [Style; 6],
    pub heading_glyphs: bool,
    /// Render a single source newline as a line break rather than a space.
    pub hard_breaks: bool,
    /// Dimmed style for markdown punctuation shown raw in the active block
    /// (e.g. `**`, `#`, backticks, link brackets).
    pub marker: Style,
    pub code_inline: Style,
    pub link: Style,
    pub quote: Style,
    pub quote_bar: Style,
    pub list_marker: Style,
    pub task_done: Style,
    pub task_todo: Style,
    pub rule: Style,
    pub table_header: Style,
    pub table_border: Style,
    pub html: Style,
    pub keyword_note: Style,
    pub keyword_warn: Style,
    pub keyword_error: Style,
    pub keyword_misc: Style,
    /// Background colour for selected text.
    pub selection: Color,
}

impl MarkdownTheme {
    pub fn from_variant(name: &str) -> Self {
        match variant_from_name(name).unwrap_or_else(detect_terminal_variant) {
            ThemeVariant::Dark => Self::dark(),
            ThemeVariant::Light => Self::light(),
        }
    }

    pub fn default_syntax_theme(&self) -> &'static str {
        match self.variant {
            ThemeVariant::Dark => "base16-ocean.dark",
            ThemeVariant::Light => "InspiredGitHub",
        }
    }

    /// Style for a heading of the given 1-based level.
    pub fn heading(&self, level: u8) -> Style {
        self.headings[(level.saturating_sub(1)).min(5) as usize]
    }

    /// Apply `element -> "#rrggbb"` overrides from config. Unknown keys and
    /// unparseable colours are ignored.
    pub fn apply_overrides(&mut self, overrides: &HashMap<String, String>) {
        // The background rewrites every element's background (including the
        // help background), so it must go first: HashMap iteration order is
        // random, and a specific override like `help_bg` has to win.
        if let Some(color) = ["background", "editor_bg", "editor_background"]
            .iter()
            .find_map(|key| overrides.get(*key))
            .and_then(|value| parse_hex(value))
        {
            self.set_background(color);
        }
        for (key, value) in overrides {
            let Some(color) = parse_hex(value) else {
                continue;
            };
            match key.as_str() {
                "text" => self.text = self.text.fg(color),
                // Applied above, before everything else.
                "background" | "editor_bg" | "editor_background" => {}
                "active_line" | "active_line_bg" => {
                    self.active_line = color;
                    self.gutter_current = self.gutter_current.bg(color);
                }
                "gutter" => self.gutter = self.gutter.fg(color),
                "gutter_current" => self.gutter_current = self.gutter_current.fg(color),
                "status" => self.status = self.status.bg(color),
                "status_fg" => self.status = self.status.fg(color),
                "help_bg" | "help_background" => self.help_background = color,
                "heading1" => self.headings[0] = self.headings[0].fg(color),
                "heading2" => self.headings[1] = self.headings[1].fg(color),
                "heading3" => self.headings[2] = self.headings[2].fg(color),
                "heading4" => self.headings[3] = self.headings[3].fg(color),
                "heading5" => self.headings[4] = self.headings[4].fg(color),
                "heading6" => self.headings[5] = self.headings[5].fg(color),
                "code" => self.code_inline = self.code_inline.fg(color),
                "code_bg" | "code_background" => self.code_inline = self.code_inline.bg(color),
                "link" => self.link = self.link.fg(color),
                "quote" => self.quote = self.quote.fg(color),
                "quote_bar" => self.quote_bar = self.quote_bar.fg(color),
                "marker" => self.marker = self.marker.fg(color),
                "list" => self.list_marker = self.list_marker.fg(color),
                "task_done" => self.task_done = self.task_done.fg(color),
                "task_todo" => self.task_todo = self.task_todo.fg(color),
                "rule" => self.rule = self.rule.fg(color),
                "table_header" => self.table_header = self.table_header.fg(color),
                "table_border" => self.table_border = self.table_border.fg(color),
                "html" => self.html = self.html.fg(color),
                "keyword" | "keyword_misc" => self.keyword_misc = self.keyword_misc.fg(color),
                "keyword_note" => self.keyword_note = self.keyword_note.fg(color),
                "keyword_warn" | "keyword_warning" => {
                    self.keyword_warn = self.keyword_warn.fg(color)
                }
                "keyword_error" => self.keyword_error = self.keyword_error.fg(color),
                "selection" => self.selection = color,
                _ => {}
            }
        }
    }

    fn set_background(&mut self, color: Color) {
        self.background = color;
        self.text = self.text.bg(color);
        for heading in &mut self.headings {
            *heading = heading.bg(color);
        }
        self.marker = self.marker.bg(color);
        self.link = self.link.bg(color);
        self.quote = self.quote.bg(color);
        self.quote_bar = self.quote_bar.bg(color);
        self.list_marker = self.list_marker.bg(color);
        self.task_done = self.task_done.bg(color);
        self.task_todo = self.task_todo.bg(color);
        self.rule = self.rule.bg(color);
        self.table_header = self.table_header.bg(color);
        self.table_border = self.table_border.bg(color);
        self.html = self.html.bg(color);
        self.keyword_note = self.keyword_note.bg(color);
        self.keyword_warn = self.keyword_warn.bg(color);
        self.keyword_error = self.keyword_error.bg(color);
        self.keyword_misc = self.keyword_misc.bg(color);
        self.gutter = self.gutter.bg(color);
        self.help_background = color;
    }
}

impl Default for MarkdownTheme {
    fn default() -> Self {
        Self::dark()
    }
}

impl MarkdownTheme {
    fn dark() -> Self {
        let fg = |r, g, b| Style::new().fg(rgb(r, g, b));
        let background = Color::Indexed(235);
        let active_line = Color::Indexed(236);
        Self {
            variant: ThemeVariant::Dark,
            background,
            active_line,
            gutter: fg(0x7b, 0x81, 0x98).bg(background),
            gutter_current: fg(0xe6, 0xe6, 0xf0)
                .bg(active_line)
                .add_modifier(Modifier::BOLD),
            status: Style::new().bg(Color::Indexed(236)).fg(Color::White),
            help_background: background,
            // Explicit light foreground rather than the terminal default, so
            // body text reads consistently and never looks dim/gray next to
            // styled runs. (Tuned for a dark terminal; config can override via
            // the `text` key.)
            text: fg(0xe6, 0xe6, 0xf0).bg(background),
            // A vivid per-level rainbow (tokyonight/tree-sitter flavour) that
            // downgrades to distinct 256-colour indices; H1 is the most
            // prominent (bold + underline).
            headings: [
                fg(0x7a, 0xa2, 0xf7)
                    .bg(background)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED), // H1 blue
                fg(0xbb, 0x9a, 0xf7)
                    .bg(background)
                    .add_modifier(Modifier::BOLD), // H2 purple
                fg(0x7d, 0xcf, 0xff)
                    .bg(background)
                    .add_modifier(Modifier::BOLD), // H3 cyan
                fg(0x9e, 0xce, 0x6a)
                    .bg(background)
                    .add_modifier(Modifier::BOLD), // H4 green
                fg(0xe0, 0xaf, 0x68)
                    .bg(background)
                    .add_modifier(Modifier::BOLD), // H5 yellow
                fg(0xf7, 0x76, 0x8e)
                    .bg(background)
                    .add_modifier(Modifier::BOLD), // H6 red
            ],
            heading_glyphs: true,
            hard_breaks: false,
            marker: fg(0x6c, 0x70, 0x86).bg(background),
            code_inline: Style::new()
                .fg(rgb(0x50, 0xfa, 0x7b))
                .bg(Color::Indexed(236)),
            link: fg(0x8b, 0xe9, 0xfd)
                .bg(background)
                .add_modifier(Modifier::UNDERLINED),
            quote: fg(0x9a, 0x9a, 0xb0)
                .bg(background)
                .add_modifier(Modifier::ITALIC),
            quote_bar: fg(0x62, 0x72, 0xa4).bg(background),
            list_marker: fg(0x8b, 0xe9, 0xfd).bg(background),
            task_done: fg(0x50, 0xfa, 0x7b).bg(background),
            task_todo: fg(0x6c, 0x6c, 0x80).bg(background),
            rule: fg(0x62, 0x72, 0xa4).bg(background),
            table_header: Style::new().bg(background).add_modifier(Modifier::BOLD),
            table_border: fg(0x62, 0x72, 0xa4).bg(background),
            html: fg(0x6c, 0x6c, 0x80)
                .bg(background)
                .add_modifier(Modifier::DIM),
            keyword_note: fg(0x8b, 0xe9, 0xfd)
                .bg(background)
                .add_modifier(Modifier::BOLD),
            keyword_warn: fg(0xf1, 0xfa, 0x8c)
                .bg(background)
                .add_modifier(Modifier::BOLD),
            keyword_error: fg(0xff, 0x55, 0x55)
                .bg(background)
                .add_modifier(Modifier::BOLD),
            keyword_misc: fg(0xff, 0xb8, 0x6c)
                .bg(background)
                .add_modifier(Modifier::BOLD),
            selection: rgb(0x33, 0x4a, 0x6b),
        }
    }

    fn light() -> Self {
        let fg = |r, g, b| Style::new().fg(rgb(r, g, b));
        let background = Color::Indexed(255);
        let active_line = Color::Indexed(254);
        Self {
            variant: ThemeVariant::Light,
            background,
            active_line,
            gutter: fg(0x6b, 0x72, 0x80).bg(background),
            gutter_current: fg(0x1f, 0x29, 0x37)
                .bg(active_line)
                .add_modifier(Modifier::BOLD),
            status: Style::new()
                .bg(Color::Indexed(254))
                .fg(rgb(0x1f, 0x29, 0x37)),
            help_background: background,
            text: fg(0x1f, 0x29, 0x37).bg(background),
            headings: [
                fg(0x00, 0x5f, 0x87)
                    .bg(background)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                fg(0x9a, 0x00, 0x5f)
                    .bg(background)
                    .add_modifier(Modifier::BOLD),
                fg(0x5f, 0x00, 0xaf)
                    .bg(background)
                    .add_modifier(Modifier::BOLD),
                fg(0x00, 0x87, 0x5f)
                    .bg(background)
                    .add_modifier(Modifier::BOLD),
                fg(0x87, 0x5f, 0x00)
                    .bg(background)
                    .add_modifier(Modifier::BOLD),
                fg(0xaf, 0x5f, 0x00)
                    .bg(background)
                    .add_modifier(Modifier::BOLD),
            ],
            heading_glyphs: true,
            hard_breaks: false,
            marker: fg(0x8a, 0x8f, 0x98).bg(background),
            code_inline: Style::new()
                .fg(rgb(0x00, 0x5f, 0x5f))
                .bg(Color::Indexed(254)),
            link: fg(0x00, 0x5f, 0xaf)
                .bg(background)
                .add_modifier(Modifier::UNDERLINED),
            quote: fg(0x5f, 0x6b, 0x7a)
                .bg(background)
                .add_modifier(Modifier::ITALIC),
            quote_bar: fg(0x8a, 0x8f, 0x98).bg(background),
            list_marker: fg(0x00, 0x5f, 0xaf).bg(background),
            task_done: fg(0x00, 0x87, 0x5f).bg(background),
            task_todo: fg(0x8a, 0x8f, 0x98).bg(background),
            rule: fg(0x8a, 0x8f, 0x98).bg(background),
            table_header: Style::new().bg(background).add_modifier(Modifier::BOLD),
            table_border: fg(0x8a, 0x8f, 0x98).bg(background),
            html: fg(0x8a, 0x8f, 0x98)
                .bg(background)
                .add_modifier(Modifier::DIM),
            keyword_note: fg(0x00, 0x5f, 0xaf)
                .bg(background)
                .add_modifier(Modifier::BOLD),
            keyword_warn: fg(0x87, 0x5f, 0x00)
                .bg(background)
                .add_modifier(Modifier::BOLD),
            keyword_error: fg(0xaf, 0x00, 0x00)
                .bg(background)
                .add_modifier(Modifier::BOLD),
            keyword_misc: fg(0xaf, 0x5f, 0x00)
                .bg(background)
                .add_modifier(Modifier::BOLD),
            selection: rgb(0xc7, 0xdc, 0xff),
        }
    }
}

fn detect_terminal_variant() -> ThemeVariant {
    for key in ["MARQI_THEME", "TERMINAL_THEME", "THEME"] {
        if let Ok(value) = env::var(key)
            && let Some(variant) = variant_from_name(&value)
        {
            return variant;
        }
    }
    // Ask the terminal for its actual background (OSC 11) before falling back
    // to the COLORFGBG hint, which is rarely set and goes stale when the user
    // switches terminal themes.
    if let Some((r, g, b)) = crate::color::terminal_background() {
        return if crate::color::rgb_is_light(r, g, b) {
            ThemeVariant::Light
        } else {
            ThemeVariant::Dark
        };
    }
    env::var("COLORFGBG")
        .ok()
        .and_then(|value| variant_from_colorfgbg(&value))
        .unwrap_or(ThemeVariant::Dark)
}

fn variant_from_name(value: &str) -> Option<ThemeVariant> {
    let value = value.trim().to_ascii_lowercase();
    if value.contains("light") {
        Some(ThemeVariant::Light)
    } else if value.contains("dark") {
        Some(ThemeVariant::Dark)
    } else {
        None
    }
}

fn variant_from_colorfgbg(value: &str) -> Option<ThemeVariant> {
    let bg = value
        .split([';', ':', ','])
        .next_back()?
        .trim()
        .parse::<u8>()
        .ok()?;
    Some(if ansi_background_is_light(bg) {
        ThemeVariant::Light
    } else {
        ThemeVariant::Dark
    })
}

fn ansi_background_is_light(index: u8) -> bool {
    match index {
        0..=15 => matches!(index, 7 | 9..=15),
        16..=231 => {
            let idx = index - 16;
            let r = idx / 36;
            let g = (idx % 36) / 6;
            let b = idx % 6;
            let level = |v: u8| [0u16, 95, 135, 175, 215, 255][v as usize];
            let lum =
                299 * u32::from(level(r)) + 587 * u32::from(level(g)) + 114 * u32::from(level(b));
            lum >= 128_000
        }
        232..=255 => {
            let intensity = 8u16 + 10 * (index as u16 - 232);
            intensity >= 128
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_extended_theme_keys() {
        let mut theme = MarkdownTheme::default();
        let overrides = HashMap::from([
            ("quote_bar".to_string(), "#112233".to_string()),
            ("table_border".to_string(), "#445566".to_string()),
            ("task_todo".to_string(), "#778899".to_string()),
            ("code_background".to_string(), "#010203".to_string()),
            ("background".to_string(), "#020406".to_string()),
            ("keyword_warn".to_string(), "#abcdef".to_string()),
        ]);

        theme.apply_overrides(&overrides);

        assert_eq!(theme.quote_bar.fg, parse_hex("#112233"));
        assert_eq!(theme.table_border.fg, parse_hex("#445566"));
        assert_eq!(theme.task_todo.fg, parse_hex("#778899"));
        assert_eq!(theme.code_inline.bg, parse_hex("#010203"));
        assert_eq!(Some(theme.background), parse_hex("#020406"));
        assert_eq!(theme.keyword_warn.fg, parse_hex("#abcdef"));
    }

    #[test]
    fn light_variant_uses_dark_body_text() {
        let theme = MarkdownTheme::from_variant("light");
        assert_eq!(Some(theme.background), Some(Color::Indexed(255)));
        assert_eq!(theme.text.fg, parse_hex("#1f2937"));
    }

    #[test]
    fn colorfgbg_hint_detects_terminal_variant() {
        assert_eq!(variant_from_colorfgbg("15;0"), Some(ThemeVariant::Dark));
        assert_eq!(variant_from_colorfgbg("0;15"), Some(ThemeVariant::Light));
        assert_eq!(variant_from_colorfgbg("0;255"), Some(ThemeVariant::Light));
    }
}
