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

pub struct CodeHighlighter {
    syntax_set: SyntaxSet,
    theme: Theme,
    bg: Color,
    /// Memoised highlight results keyed by (lang, code, width). Highlighting is
    /// the costly part of re-rendering, so unchanged code blocks are reused.
    /// The full key is stored (not a hash) so a collision can never serve
    /// another block's rendering; memory stays bounded by [`CACHE_LIMIT`].
    cache: RefCell<HashMap<(String, String, usize), Vec<Line<'static>>>>,
}

impl CodeHighlighter {
    /// Build a highlighter using the named syntect theme (falling back to a
    /// sensible default when `name` is `None` or unknown).
    pub fn new(name: Option<&str>) -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = name
            .and_then(|n| theme_set.themes.get(n))
            .or_else(|| theme_set.themes.get("base16-ocean.dark"))
            .or_else(|| theme_set.themes.values().next())
            .cloned()
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
    /// rows at most `width` columns wide. Results are memoised.
    pub fn highlight(&self, lang: &str, code: &str, width: usize) -> Vec<Line<'static>> {
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

    fn highlight_uncached(&self, lang: &str, code: &str, width: usize) -> Vec<Line<'static>> {
        let syntax = self
            .syntax_set
            .find_syntax_by_token(lang)
            .or_else(|| self.syntax_set.find_syntax_by_extension(lang))
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, &self.theme);

        let mut out = Vec::new();
        for line in LinesWithEndings::from(code) {
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
            self.emit_wrapped(graphemes, width, &mut out);
        }
        if out.is_empty() {
            out.push(self.pad_row(Vec::new(), 0, width));
        }
        out
    }

    /// Hard-wrap one logical code line into one or more padded rows.
    fn emit_wrapped(
        &self,
        graphemes: Vec<(String, Style)>,
        width: usize,
        out: &mut Vec<Line<'static>>,
    ) {
        let mut row: Vec<(String, Style)> = Vec::new();
        let mut col = 0usize;
        for (g, style) in graphemes {
            let w = UnicodeWidthStr::width(g.as_str());
            if col + w > width && !row.is_empty() {
                let taken = std::mem::take(&mut row);
                out.push(self.pad_row(taken, col, width));
                col = 0;
            }
            row.push((g, style));
            col += w;
        }
        out.push(self.pad_row(row, col, width));
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
