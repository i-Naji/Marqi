//! Fenced-code-block syntax highlighting via syntect, converted to ratatui
//! spans.
//!
//! The syntax and theme sets are loaded once (they are not cheap) and reused
//! for every code block. Each code line is highlighted, hard-wrapped to the
//! available width, and padded so the block reads as a solid panel.

use std::cell::RefCell;
use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::color::rgb;

/// Drop the highlight cache once it grows past this many distinct code blocks.
const CACHE_LIMIT: usize = 2048;

/// A highlighted row and the 0-based literal line it begins (`None` for the
/// continuation rows of a hard-wrapped line).
type HighlightedRow = (Line<'static>, Option<usize>);

pub struct CodeHighlighter {
    syntax_set: SyntaxSet,
    theme: Theme,
    bg: Color,
    /// Memoised highlight results keyed by (lang, code, width). Highlighting is
    /// the costly part of re-rendering, so unchanged code blocks are reused.
    /// The full key is stored (not a hash) so a collision can never serve
    /// another block's rendering; memory stays bounded by [`CACHE_LIMIT`].
    cache: RefCell<HashMap<(String, String, usize), Vec<HighlightedRow>>>,
}

/// Canonical `.tmTheme` ports bundled for the named palette families (see
/// `MarkdownTheme::default_syntax_theme`). Embedded at build time; only the
/// selected one is ever parsed, so the cost is a sub-millisecond one-time
/// plist parse for one theme.
fn bundled_theme(name: &str) -> Option<&'static [u8]> {
    let bytes: &'static [u8] = match name {
        "catppuccin-mocha" => include_bytes!("syntax_themes/catppuccin-mocha.tmTheme"),
        "catppuccin-latte" => include_bytes!("syntax_themes/catppuccin-latte.tmTheme"),
        "tokyonight-night" => include_bytes!("syntax_themes/tokyonight-night.tmTheme"),
        "tokyonight-day" => include_bytes!("syntax_themes/tokyonight-day.tmTheme"),
        "onehalf-dark" => include_bytes!("syntax_themes/onehalf-dark.tmTheme"),
        "onehalf-light" => include_bytes!("syntax_themes/onehalf-light.tmTheme"),
        "gruvbox-dark" => include_bytes!("syntax_themes/gruvbox-dark.tmTheme"),
        "gruvbox-light" => include_bytes!("syntax_themes/gruvbox-light.tmTheme"),
        "dracula" => include_bytes!("syntax_themes/dracula.tmTheme"),
        "nord" => include_bytes!("syntax_themes/nord.tmTheme"),
        _ => return None,
    };
    Some(bytes)
}

impl CodeHighlighter {
    pub fn has_theme(name: &str) -> bool {
        static DEFAULTS: std::sync::LazyLock<ThemeSet> =
            std::sync::LazyLock::new(ThemeSet::load_defaults);
        bundled_theme(name).is_some() || DEFAULTS.themes.contains_key(name)
    }

    /// Build a highlighter using the named syntect theme — a bundled port, or
    /// one of syntect's defaults — falling back to a sensible default when
    /// `name` is `None` or unknown.
    pub fn new(name: Option<&str>) -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = name
            .and_then(|n| {
                bundled_theme(n)
                    .and_then(|bytes| {
                        ThemeSet::load_from_reader(&mut std::io::Cursor::new(bytes)).ok()
                    })
                    .or_else(|| theme_set.themes.get(n).cloned())
            })
            .or_else(|| theme_set.themes.get("base16-ocean.dark").cloned())
            .or_else(|| theme_set.themes.values().next().cloned())
            .expect("syntect ships default themes");
        let bg = theme
            .settings
            .background
            .map(|c| rgb(c.r, c.g, c.b))
            .unwrap_or(Color::Indexed(235));
        Self {
            syntax_set,
            theme,
            bg,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Highlight `code` (a fenced block's literal) for `lang`, producing styled
    /// rows at most `width` columns wide. Each row carries the 0-based literal
    /// line it begins; hard-wrapped continuation rows carry `None`. Results are
    /// memoised.
    pub fn highlight(&self, lang: &str, code: &str, width: usize) -> Vec<HighlightedRow> {
        let width = width.max(1);

        let key = (lang.to_string(), code.to_string(), width);
        if let Some(cached) = self.cache.borrow().get(&key) {
            return cached.clone();
        }

        let lines = self.highlight_uncached(lang, code, width);
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, lines.clone());
        lines
    }

    fn highlight_uncached(&self, lang: &str, code: &str, width: usize) -> Vec<HighlightedRow> {
        let syntax = self
            .syntax_set
            .find_syntax_by_token(lang)
            .or_else(|| self.syntax_set.find_syntax_by_extension(lang))
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, &self.theme);

        let mut out = Vec::new();
        for (i, line) in LinesWithEndings::from(code).enumerate() {
            // On a highlight error (fancy-regex can fail on pathological
            // lines) fall back to the unstyled text — never drop the line.
            let ranges = highlighter
                .highlight_line(line, &self.syntax_set)
                .unwrap_or_else(|_| vec![(syntect::highlighting::Style::default(), line)]);
            // Flatten into (grapheme, style) for width-aware hard wrapping.
            let mut graphemes: Vec<(String, Style)> = Vec::new();
            for (syn_style, text) in ranges {
                let style = self.convert(syn_style);
                for g in text.trim_end_matches(['\n', '\r']).graphemes(true) {
                    graphemes.push((g.to_string(), style));
                }
            }
            self.emit_wrapped(graphemes, width, i, &mut out);
        }
        if out.is_empty() {
            out.push((self.pad_row(Vec::new(), 0, width), None));
        }
        out
    }

    /// Hard-wrap one logical code line into one or more padded rows; only the
    /// first row is tagged with the line's index.
    fn emit_wrapped(
        &self,
        graphemes: Vec<(String, Style)>,
        width: usize,
        source: usize,
        out: &mut Vec<HighlightedRow>,
    ) {
        let mut row: Vec<(String, Style)> = Vec::new();
        let mut col = 0usize;
        let mut first = true;
        for (g, style) in graphemes {
            let w = UnicodeWidthStr::width(g.as_str());
            if col + w > width && !row.is_empty() {
                let taken = std::mem::take(&mut row);
                out.push((self.pad_row(taken, col, width), first.then_some(source)));
                first = false;
                col = 0;
            }
            row.push((g, style));
            col += w;
        }
        out.push((self.pad_row(row, col, width), first.then_some(source)));
    }

    /// Coalesce same-style runs into spans and pad the row to `width` with the
    /// code background.
    fn pad_row(&self, row: Vec<(String, Style)>, used: usize, width: usize) -> Line<'static> {
        let mut line = super::merge_spans(row.into_iter());
        if used < width {
            line.spans.push(Span::styled(
                " ".repeat(width - used),
                Style::new().bg(self.bg),
            ));
        }
        line
    }

    /// Convert a syntect style to a ratatui style (fg + the code bg + modifiers).
    fn convert(&self, s: syntect::highlighting::Style) -> Style {
        let mut style = Style::new()
            .fg(rgb(s.foreground.r, s.foreground.g, s.foreground.b))
            .bg(self.bg);
        if s.font_style.contains(FontStyle::BOLD) {
            style = style.add_modifier(Modifier::BOLD);
        }
        if s.font_style.contains(FontStyle::ITALIC) {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if s.font_style.contains(FontStyle::UNDERLINE) {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        style
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::MarkdownTheme;
    use crate::markdown::theme::{ThemeName, ThemeVariant};

    const BUNDLED: [&str; 10] = [
        "catppuccin-mocha",
        "catppuccin-latte",
        "tokyonight-night",
        "tokyonight-day",
        "onehalf-dark",
        "onehalf-light",
        "gruvbox-dark",
        "gruvbox-light",
        "dracula",
        "nord",
    ];

    #[test]
    fn every_bundled_theme_parses() {
        for name in BUNDLED {
            let bytes = bundled_theme(name).expect("bundled");
            let theme = ThemeSet::load_from_reader(&mut std::io::Cursor::new(bytes))
                .unwrap_or_else(|e| panic!("{name}: invalid tmTheme: {e}"));
            assert!(
                theme.settings.background.is_some(),
                "{name}: a code theme needs a background"
            );
        }
    }

    #[test]
    fn every_palette_pairing_resolves_and_highlights() {
        for name in [
            ThemeName::Marqi,
            ThemeName::OneDark,
            ThemeName::Github,
            ThemeName::Catppuccin,
            ThemeName::TokyoNight,
            ThemeName::Gruvbox,
            ThemeName::Nord,
            ThemeName::Dracula,
            ThemeName::Solarized,
        ] {
            for variant in [ThemeVariant::Dark, ThemeVariant::Light] {
                let pairing = MarkdownTheme::named(name, variant).default_syntax_theme();
                let hl = CodeHighlighter::new(Some(pairing));
                let rows = hl.highlight("rust", "fn main() { let x = 1; }\n", 40);
                assert!(
                    !rows.is_empty(),
                    "{name:?}/{variant:?} via {pairing:?} highlights"
                );
            }
        }
    }
}
