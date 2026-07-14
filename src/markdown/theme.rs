//! Visual theme for rendered markdown: a ratatui [`Style`] per element.
//!
//! Themes come in named palette families ([`ThemeName`]), each with a dark
//! and a light face selected by [`ThemeVariant`] — two independent axes
//! (`theme.name` × `theme.variant` in config). Every face derives its ~25
//! element roles from a small seed [`Palette`] of canonical colours, and can
//! be overridden per element from `[theme.markdown]` config via
//! [`MarkdownTheme::apply_overrides`].

use std::collections::HashMap;
use std::env;

use ratatui::style::{Color, Modifier, Style};

use crate::color::{parse_hex, rgb};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeVariant {
    Dark,
    Light,
}

impl ThemeVariant {
    /// Display name for the settings menu.
    pub fn label(self) -> &'static str {
        match self {
            ThemeVariant::Dark => "Dark",
            ThemeVariant::Light => "Light",
        }
    }
}

/// Built-in palette families. `Marqi` is the original hand-tuned look; the
/// rest follow the canonical published palettes of the most popular editor
/// and terminal themes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeName {
    Marqi,
    OneDark,
    Github,
    Catppuccin,
    TokyoNight,
    Gruvbox,
    Nord,
    Dracula,
    Solarized,
}

impl ThemeName {
    /// Every built-in palette family, in settings-menu order.
    pub const ALL: [ThemeName; 9] = [
        ThemeName::Marqi,
        ThemeName::OneDark,
        ThemeName::Github,
        ThemeName::Catppuccin,
        ThemeName::TokyoNight,
        ThemeName::Gruvbox,
        ThemeName::Nord,
        ThemeName::Dracula,
        ThemeName::Solarized,
    ];

    /// Display name for the settings menu.
    pub fn label(self) -> &'static str {
        match self {
            ThemeName::Marqi => "marqi",
            ThemeName::OneDark => "One Dark",
            ThemeName::Github => "GitHub",
            ThemeName::Catppuccin => "Catppuccin",
            ThemeName::TokyoNight => "Tokyo Night",
            ThemeName::Gruvbox => "Gruvbox",
            ThemeName::Nord => "Nord",
            ThemeName::Dracula => "Dracula",
            ThemeName::Solarized => "Solarized",
        }
    }

    pub fn config_name(self) -> &'static str {
        match self {
            ThemeName::Marqi => "marqi",
            ThemeName::OneDark => "onedark",
            ThemeName::Github => "github",
            ThemeName::Catppuccin => "catppuccin",
            ThemeName::TokyoNight => "tokyonight",
            ThemeName::Gruvbox => "gruvbox",
            ThemeName::Nord => "nord",
            ThemeName::Dracula => "dracula",
            ThemeName::Solarized => "solarized",
        }
    }
}

pub struct MarkdownTheme {
    pub name: ThemeName,
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
    pub fn is_known_name(value: &str) -> bool {
        matches!(value.trim().to_ascii_lowercase().as_str(), "" | "default")
            || theme_from_name(value).is_some()
    }

    pub fn is_override_key(value: &str) -> bool {
        matches!(
            value,
            "text"
                | "background"
                | "editor_bg"
                | "editor_background"
                | "active_line"
                | "active_line_bg"
                | "gutter"
                | "gutter_current"
                | "status"
                | "status_fg"
                | "help_bg"
                | "help_background"
                | "heading1"
                | "heading2"
                | "heading3"
                | "heading4"
                | "heading5"
                | "heading6"
                | "code"
                | "code_bg"
                | "code_background"
                | "link"
                | "quote"
                | "quote_bar"
                | "marker"
                | "list"
                | "task_done"
                | "task_todo"
                | "rule"
                | "table_header"
                | "table_border"
                | "html"
                | "keyword"
                | "keyword_misc"
                | "keyword_note"
                | "keyword_warn"
                | "keyword_warning"
                | "keyword_error"
                | "selection"
        )
    }

    /// Select a theme from the config's two axes: a palette family name and a
    /// variant ("auto" detects the terminal background). An empty or unknown
    /// name picks the most popular face per mode — One Dark when dark,
    /// GitHub when light.
    pub fn select(name: &str, variant: &str) -> Self {
        let variant = variant_from_name(variant).unwrap_or_else(detect_terminal_variant);
        let name = theme_from_name(name).unwrap_or(match variant {
            ThemeVariant::Dark => ThemeName::OneDark,
            ThemeVariant::Light => ThemeName::Github,
        });
        Self::named(name, variant)
    }

    pub fn named(name: ThemeName, variant: ThemeVariant) -> Self {
        match (name, variant) {
            (ThemeName::Marqi, ThemeVariant::Dark) => Self::dark(),
            (ThemeName::Marqi, ThemeVariant::Light) => Self::light(),
            _ => Self::from_palette(name, variant, palette(name, variant)),
        }
    }

    /// The syntect theme paired with this palette face for fenced code
    /// blocks. Bundled `.tmTheme` ports where syntect's defaults have no
    /// match (see `markdown::highlight`); a few light faces without a
    /// canonical port pair with a neutral light theme instead.
    pub fn default_syntax_theme(&self) -> &'static str {
        use ThemeName::*;
        let dark = self.variant == ThemeVariant::Dark;
        match self.name {
            Marqi => {
                if dark {
                    "base16-ocean.dark"
                } else {
                    "InspiredGitHub"
                }
            }
            OneDark => {
                if dark {
                    "onehalf-dark"
                } else {
                    "onehalf-light"
                }
            }
            Github => {
                if dark {
                    "onehalf-dark"
                } else {
                    "InspiredGitHub"
                }
            }
            Catppuccin => {
                if dark {
                    "catppuccin-mocha"
                } else {
                    "catppuccin-latte"
                }
            }
            TokyoNight => {
                if dark {
                    "tokyonight-night"
                } else {
                    "tokyonight-day"
                }
            }
            Gruvbox => {
                if dark {
                    "gruvbox-dark"
                } else {
                    "gruvbox-light"
                }
            }
            Nord => {
                if dark {
                    "nord"
                } else {
                    "onehalf-light"
                }
            }
            Dracula => {
                if dark {
                    "dracula"
                } else {
                    "onehalf-light"
                }
            }
            Solarized => {
                if dark {
                    "Solarized (dark)"
                } else {
                    "Solarized (light)"
                }
            }
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
            name: ThemeName::Marqi,
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
            // Restrained heading ladder: a few palette accents for major
            // sections, then text/muted/faint for lower levels.
            headings: heading_ladder(0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xe6e6f0, 0x9a9ab0, 0x6c7086)
                .map(|hex| hstyle(hex, background)),
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
            list_marker: fg(0x62, 0x72, 0xa4).bg(background),
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
            name: ThemeName::Marqi,
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
            // Restrained heading ladder: a few palette accents for major
            // sections, then text/muted/faint for lower levels.
            headings: heading_ladder(0x005f87, 0x6b46c1, 0x00796b, 0x1f2937, 0x5f6b7a, 0x8a8f98)
                .map(|hex| hstyle(hex, background)),
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
            list_marker: fg(0x8a, 0x8f, 0x98).bg(background),
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

    /// Derive the full role set from a seed palette. Headings use a restrained
    /// accent ladder for H1-H3, then recede through text/muted/faint for H4-H6.
    /// Secondary chrome stays muted so prose and links keep the foreground.
    fn from_palette(name: ThemeName, variant: ThemeVariant, p: Palette) -> Self {
        let bg = p.background;
        // The xterm-256 downgrade can collapse a subtle offset into the
        // canvas index; nudge so highlights stay visible there.
        let surface = offset_from(p.surface, bg, variant);
        let selection = offset_from(p.selection, bg, variant);
        let s = |c: Color| Style::new().fg(c).bg(bg);
        let heading = |c: Color| s(c).add_modifier(Modifier::BOLD);
        Self {
            name,
            variant,
            background: bg,
            active_line: surface,
            gutter: s(p.faint),
            gutter_current: Style::new()
                .fg(p.text)
                .bg(surface)
                .add_modifier(Modifier::BOLD),
            status: Style::new().bg(surface).fg(p.text),
            help_background: bg,
            text: s(p.text),
            // A few accents, then quieter lower levels; bold throughout and
            // no underline noise.
            headings: p.headings.map(heading),
            heading_glyphs: true,
            hard_breaks: false,
            marker: s(p.faint),
            code_inline: Style::new().fg(p.code).bg(surface),
            link: s(p.link).add_modifier(Modifier::UNDERLINED),
            quote: s(p.muted).add_modifier(Modifier::ITALIC),
            quote_bar: s(p.chrome),
            // List bullets are document chrome, not link/command accents.
            list_marker: s(p.chrome),
            task_done: s(p.green),
            task_todo: s(p.faint),
            rule: s(p.chrome),
            table_header: Style::new().bg(bg).add_modifier(Modifier::BOLD),
            table_border: s(p.chrome),
            html: s(p.faint).add_modifier(Modifier::DIM),
            keyword_note: s(p.link).add_modifier(Modifier::BOLD),
            keyword_warn: s(p.yellow).add_modifier(Modifier::BOLD),
            keyword_error: s(p.red).add_modifier(Modifier::BOLD),
            keyword_misc: s(p.orange).add_modifier(Modifier::BOLD),
            selection,
        }
    }
}

/// Linear per-channel blend of two `0xRRGGBB` colours; `t` in `0.0..=1.0`
/// moves from `a` toward `b`.
fn mix(a: u32, b: u32, t: f64) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |shift: u32| {
        let (ca, cb) = (((a >> shift) & 0xff) as f64, ((b >> shift) & 0xff) as f64);
        (ca + (cb - ca) * t).round() as u32
    };
    (lerp(16) << 16) | (lerp(8) << 8) | lerp(0)
}

/// A bold heading style: `0xRRGGBB` foreground on `bg` (for the hand-tuned
/// marqi themes, which build styles directly rather than from a seed palette).
fn hstyle(hex: u32, bg: Color) -> Style {
    Style::new()
        .fg(rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8))
        .bg(bg)
        .add_modifier(Modifier::BOLD)
}

/// H1..H6 foregrounds: H1-H3 get restrained palette accents, while H4-H6
/// step down through text, muted text, and faint chrome.
fn heading_ladder(
    primary: u32,
    secondary: u32,
    tertiary: u32,
    text: u32,
    muted: u32,
    faint: u32,
) -> [u32; 6] {
    [primary, secondary, tertiary, text, muted, faint]
}

/// A surface/selection colour, guaranteed distinct from the canvas after any
/// 256-colour downgrade: a collapsed index steps one slot lighter on dark
/// themes and one darker on light ones (adjacent on the grey ramp, adjacent
/// brightness in the colour cube).
fn offset_from(color: Color, background: Color, variant: ThemeVariant) -> Color {
    if color != background {
        return color;
    }
    match color {
        Color::Indexed(n) => Color::Indexed(match variant {
            ThemeVariant::Dark => n.saturating_add(1),
            ThemeVariant::Light => n.saturating_sub(1),
        }),
        other => other,
    }
}

/// The seed colours a named palette face provides; every [`MarkdownTheme`]
/// role derives from these (see [`MarkdownTheme::from_palette`]).
struct Palette {
    /// Editor canvas.
    background: Color,
    /// Offset surface: active line, status bar, inline-code background.
    surface: Color,
    /// Body text.
    text: Color,
    /// Secondary text (quotes).
    muted: Color,
    /// Faint chrome: markers, gutter, todo boxes, raw HTML.
    faint: Color,
    /// Structural lines: quote bars, rules, table borders.
    chrome: Color,
    /// Selection background.
    selection: Color,
    /// Links and NOTE-style keywords.
    link: Color,
    /// Inline code foreground (softened toward the body text for legibility).
    code: Color,
    /// H1..H6 foregrounds: a restrained accent ladder followed by quiet text
    /// levels (see [`heading_ladder`]).
    headings: [Color; 6],
    // Semantic accents still used by roles (keyword badges and task-done).
    // The remaining hues in each seed's `accents` array are folded into the
    // heading ladder.
    red: Color,
    orange: Color,
    yellow: Color,
    green: Color,
}

/// Canonical seed colours per family and face. Sources: the published
/// palettes of each project (One Dark/Light, GitHub Primer, Catppuccin
/// Mocha/Latte, Tokyo Night/Day, Gruvbox medium, Nord, Dracula, Solarized).
/// Faces without an upstream light palette (Nord, Dracula's Alucard) use
/// derived hues tuned for contrast on the canonical light canvas.
fn palette(name: ThemeName, variant: ThemeVariant) -> Palette {
    let c = |hex: u32| rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    let p = |background: u32,
             surface: u32,
             text: u32,
             muted: u32,
             faint: u32,
             chrome: u32,
             selection: u32,
             link: u32,
             code: u32,
             accents: [u32; 7]| Palette {
        background: c(background),
        surface: c(surface),
        text: c(text),
        muted: c(muted),
        faint: c(faint),
        chrome: c(chrome),
        selection: c(selection),
        link: c(link),
        // Pull the theme's raw code colour toward the body text so inline
        // code reads on the surface chip instead of shouting a saturated hue.
        code: c(mix(code, text, 0.4)),
        headings: heading_ladder(accents[5], accents[6], accents[4], text, muted, faint).map(c),
        red: c(accents[0]),
        orange: c(accents[1]),
        yellow: c(accents[2]),
        green: c(accents[3]),
    };
    let dark = variant == ThemeVariant::Dark;
    match name {
        // `Marqi` is built by `dark()`/`light()`, never from a seed.
        ThemeName::Marqi | ThemeName::OneDark => {
            if dark {
                p(
                    0x282c34,
                    0x2c313c,
                    0xabb2bf,
                    0x828997,
                    0x5c6370,
                    0x5c6370,
                    0x3e4451,
                    0x61afef,
                    0xe06c75,
                    [
                        0xe06c75, 0xd19a66, 0xe5c07b, 0x98c379, 0x56b6c2, 0x61afef, 0xc678dd,
                    ],
                )
            } else {
                p(
                    0xfafafa,
                    0xf0f0f1,
                    0x383a42,
                    0x696c77,
                    0xa0a1a7,
                    0xa0a1a7,
                    0xdee4ee,
                    0x4078f2,
                    0xe45649,
                    [
                        0xe45649, 0x986801, 0xc18401, 0x50a14f, 0x0184bc, 0x4078f2, 0xa626a4,
                    ],
                )
            }
        }
        ThemeName::Github => {
            if dark {
                p(
                    0x0d1117,
                    0x161b22,
                    0xe6edf3,
                    0x8b949e,
                    0x6e7681,
                    0x30363d,
                    0x1c395c,
                    0x58a6ff,
                    0xe6edf3,
                    [
                        0xf85149, 0xf0883e, 0xd29922, 0x3fb950, 0x79c0ff, 0x58a6ff, 0xbc8cff,
                    ],
                )
            } else {
                p(
                    0xffffff,
                    0xf6f8fa,
                    0x24292f,
                    0x57606a,
                    0x8c959f,
                    0xd0d7de,
                    0xddebff,
                    0x0969da,
                    0x24292f,
                    [
                        0xcf222e, 0xbc4c00, 0x9a6700, 0x1a7f37, 0x218bff, 0x0969da, 0x8250df,
                    ],
                )
            }
        }
        ThemeName::Catppuccin => {
            if dark {
                p(
                    0x1e1e2e,
                    0x313244,
                    0xcdd6f4,
                    0xa6adc8,
                    0x6c7086,
                    0x585b70,
                    0x45475a,
                    0x89b4fa,
                    0xa6e3a1,
                    [
                        0xf38ba8, 0xfab387, 0xf9e2af, 0xa6e3a1, 0x89dceb, 0x89b4fa, 0xcba6f7,
                    ],
                )
            } else {
                p(
                    0xeff1f5,
                    0xccd0da,
                    0x4c4f69,
                    0x6c6f85,
                    0x9ca0b0,
                    0xacb0be,
                    0xbcc0cc,
                    0x1e66f5,
                    0x40a02b,
                    [
                        0xd20f39, 0xfe640b, 0xdf8e1d, 0x40a02b, 0x04a5e5, 0x1e66f5, 0x8839ef,
                    ],
                )
            }
        }
        ThemeName::TokyoNight => {
            if dark {
                p(
                    0x1a1b26,
                    0x292e42,
                    0xc0caf5,
                    0xa9b1d6,
                    0x565f89,
                    0x3b4261,
                    0x283457,
                    0x7aa2f7,
                    0x73daca,
                    [
                        0xf7768e, 0xff9e64, 0xe0af68, 0x9ece6a, 0x7dcfff, 0x7aa2f7, 0xbb9af7,
                    ],
                )
            } else {
                p(
                    0xe1e2e7,
                    0xc4c8da,
                    0x3760bf,
                    0x6172b0,
                    0x848cb5,
                    0xa8aecb,
                    0xb6bfe2,
                    0x2e7de9,
                    0x387068,
                    [
                        0xf52a65, 0xb15c00, 0x8c6c3e, 0x587539, 0x007197, 0x2e7de9, 0x9854f1,
                    ],
                )
            }
        }
        ThemeName::Gruvbox => {
            if dark {
                p(
                    0x282828,
                    0x3c3836,
                    0xebdbb2,
                    0xd5c4a1,
                    0x928374,
                    0x665c54,
                    0x504945,
                    0x83a598,
                    0xfe8019,
                    [
                        0xfb4934, 0xfe8019, 0xfabd2f, 0xb8bb26, 0x8ec07c, 0x83a598, 0xd3869b,
                    ],
                )
            } else {
                p(
                    0xfbf1c7,
                    0xebdbb2,
                    0x3c3836,
                    0x665c54,
                    0x928374,
                    0xbdae93,
                    0xd5c4a1,
                    0x076678,
                    0xaf3a03,
                    [
                        0x9d0006, 0xaf3a03, 0xb57614, 0x79740e, 0x427b58, 0x076678, 0x8f3f71,
                    ],
                )
            }
        }
        ThemeName::Nord => {
            if dark {
                p(
                    0x2e3440,
                    0x3b4252,
                    0xd8dee9,
                    0x9fa8b8,
                    0x616e88,
                    0x4c566a,
                    0x434c5e,
                    0x88c0d0,
                    0x88c0d0,
                    [
                        0xbf616a, 0xd08770, 0xebcb8b, 0xa3be8c, 0x88c0d0, 0x81a1c1, 0xb48ead,
                    ],
                )
            } else {
                p(
                    0xeceff4,
                    0xe5e9f0,
                    0x2e3440,
                    0x4c566a,
                    0x969eaa,
                    0xaeb8ca,
                    0xd8dee9,
                    0x5e81ac,
                    0x2f7d8e,
                    [
                        0xa14a52, 0xad6a4e, 0xb08a3e, 0x6f8a55, 0x2f7d8e, 0x5e81ac, 0x8f718c,
                    ],
                )
            }
        }
        ThemeName::Dracula => {
            if dark {
                p(
                    0x282a36,
                    0x44475a,
                    0xf8f8f2,
                    0x6272a4,
                    0x6272a4,
                    0x6272a4,
                    0x44475a,
                    0x8be9fd,
                    0x50fa7b,
                    [
                        0xff5555, 0xffb86c, 0xf1fa8c, 0x50fa7b, 0x8be9fd, 0xbd93f9, 0xff79c6,
                    ],
                )
            } else {
                // Alucard-style: Dracula's hues darkened for a light canvas.
                p(
                    0xfffbeb,
                    0xf5f3e8,
                    0x1f1f23,
                    0x6c6783,
                    0x9694a0,
                    0x9694a0,
                    0xe8e4f6,
                    0x036a96,
                    0x14710a,
                    [
                        0xcb3a2a, 0xa34d14, 0x846e15, 0x14710a, 0x036a96, 0x644ac9, 0xa3144d,
                    ],
                )
            }
        }
        ThemeName::Solarized => {
            if dark {
                p(
                    0x002b36,
                    0x073642,
                    0x839496,
                    0x586e75,
                    0x586e75,
                    0x586e75,
                    0x073642,
                    0x268bd2,
                    0x2aa198,
                    [
                        0xdc322f, 0xcb4b16, 0xb58900, 0x859900, 0x2aa198, 0x268bd2, 0x6c71c4,
                    ],
                )
            } else {
                p(
                    0xfdf6e3,
                    0xeee8d5,
                    0x657b83,
                    0x93a1a1,
                    0x93a1a1,
                    0x93a1a1,
                    0xeee8d5,
                    0x268bd2,
                    0x2aa198,
                    [
                        0xdc322f, 0xcb4b16, 0xb58900, 0x859900, 0x2aa198, 0x268bd2, 0x6c71c4,
                    ],
                )
            }
        }
    }
}

/// Parse a palette-family name from config. Accepts the common aliases and
/// spellings; mode words in the name ("onedark", "latte") select the family
/// only — the face still follows `theme.variant`.
fn theme_from_name(value: &str) -> Option<ThemeName> {
    let value: String = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    Some(match value.as_str() {
        "marqi" => ThemeName::Marqi,
        "onedark" | "onelight" | "one" | "onedarkpro" | "atomone" => ThemeName::OneDark,
        "github" | "githubdark" | "githublight" | "primer" => ThemeName::Github,
        "catppuccin"
        | "catppuccinmocha"
        | "catppuccinlatte"
        | "catppuccinfrappe"
        | "catppuccinmacchiato"
        | "mocha"
        | "latte"
        | "frappe"
        | "macchiato" => ThemeName::Catppuccin,
        "tokyonight" | "tokyonightnight" | "tokyonightday" | "tokyonightstorm" | "tokyo" => {
            ThemeName::TokyoNight
        }
        "gruvbox" | "gruvboxdark" | "gruvboxlight" => ThemeName::Gruvbox,
        "nord" => ThemeName::Nord,
        "dracula" | "alucard" => ThemeName::Dracula,
        "solarized" | "solarizeddark" | "solarizedlight" => ThemeName::Solarized,
        _ => return None,
    })
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
    fn mix_blends_channels() {
        assert_eq!(mix(0x000000, 0xffffff, 0.5), 0x808080);
        assert_eq!(mix(0x102030, 0x102030, 0.7), 0x102030);
        assert_eq!(mix(0xff0000, 0x00ff00, 0.0), 0xff0000);
        assert_eq!(mix(0xff0000, 0x00ff00, 1.0), 0x00ff00);
    }

    #[test]
    fn heading_ladder_keeps_accents_and_quiet_levels() {
        let h = heading_ladder(0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf, 0x828997, 0x5c6370);
        assert_eq!(h[0], 0x61afef, "H1 keeps the primary accent");
        assert_eq!(h[1], 0xc678dd, "H2 keeps the secondary accent");
        assert_eq!(h[2], 0x56b6c2, "H3 keeps the tertiary accent");
        assert_eq!(h[3], 0xabb2bf, "H4 returns to body text");
        assert_eq!(h[5], 0x5c6370, "H6 recedes into chrome");
        let distinct: std::collections::HashSet<_> = h.iter().collect();
        assert_eq!(distinct.len(), 6, "all six levels are distinct: {h:x?}");
    }

    #[test]
    fn no_theme_underlines_its_headings() {
        for name in ThemeName::ALL {
            for variant in [ThemeVariant::Dark, ThemeVariant::Light] {
                let t = MarkdownTheme::named(name, variant);
                assert!(
                    !t.headings[0].add_modifier.contains(Modifier::UNDERLINED),
                    "{name:?}/{variant:?}: H1 should not be underlined"
                );
            }
        }
    }

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
        let theme = MarkdownTheme::named(ThemeName::Marqi, ThemeVariant::Light);
        assert_eq!(Some(theme.background), Some(Color::Indexed(255)));
        assert_eq!(theme.text.fg, parse_hex("#1f2937"));
    }

    #[test]
    fn colorfgbg_hint_detects_terminal_variant() {
        assert_eq!(variant_from_colorfgbg("15;0"), Some(ThemeVariant::Dark));
        assert_eq!(variant_from_colorfgbg("0;15"), Some(ThemeVariant::Light));
        assert_eq!(variant_from_colorfgbg("0;255"), Some(ThemeVariant::Light));
    }

    const ALL: [ThemeName; 9] = [
        ThemeName::Marqi,
        ThemeName::OneDark,
        ThemeName::Github,
        ThemeName::Catppuccin,
        ThemeName::TokyoNight,
        ThemeName::Gruvbox,
        ThemeName::Nord,
        ThemeName::Dracula,
        ThemeName::Solarized,
    ];

    #[test]
    fn theme_names_and_aliases_resolve() {
        for (alias, expected) in [
            ("marqi", ThemeName::Marqi),
            ("One Dark", ThemeName::OneDark),
            ("onedark", ThemeName::OneDark),
            ("one-light", ThemeName::OneDark),
            ("GitHub", ThemeName::Github),
            ("github-dark", ThemeName::Github),
            ("catppuccin", ThemeName::Catppuccin),
            ("Catppuccin Mocha", ThemeName::Catppuccin),
            ("latte", ThemeName::Catppuccin),
            ("tokyo-night", ThemeName::TokyoNight),
            ("tokyonight_day", ThemeName::TokyoNight),
            ("gruvbox", ThemeName::Gruvbox),
            ("Gruvbox Light", ThemeName::Gruvbox),
            ("nord", ThemeName::Nord),
            ("dracula", ThemeName::Dracula),
            ("alucard", ThemeName::Dracula),
            ("solarized", ThemeName::Solarized),
        ] {
            assert_eq!(theme_from_name(alias), Some(expected), "alias {alias:?}");
        }
        assert_eq!(theme_from_name(""), None);
        assert_eq!(theme_from_name("default"), None);
        assert_eq!(theme_from_name("no-such-theme"), None);
    }

    #[test]
    fn unset_name_defaults_to_the_most_popular_face_per_mode() {
        let dark = MarkdownTheme::select("", "dark");
        assert_eq!(dark.name, ThemeName::OneDark);
        assert_eq!(dark.background, rgb(0x28, 0x2c, 0x34));

        let light = MarkdownTheme::select("default", "light");
        assert_eq!(light.name, ThemeName::Github);
        assert_eq!(light.background, rgb(0xff, 0xff, 0xff));

        // The mode words in a family name never override `variant`.
        let day = MarkdownTheme::select("tokyonight_night", "light");
        assert_eq!(day.name, ThemeName::TokyoNight);
        assert_eq!(day.variant, ThemeVariant::Light);
        assert_eq!(day.background, rgb(0xe1, 0xe2, 0xe7));
    }

    #[test]
    fn every_face_is_usable() {
        for name in ALL {
            for variant in [ThemeVariant::Dark, ThemeVariant::Light] {
                let theme = MarkdownTheme::named(name, variant);
                assert_eq!(theme.name, name);
                assert_eq!(theme.variant, variant);
                assert_ne!(
                    theme.text.fg,
                    Some(theme.background),
                    "{name:?}/{variant:?}: text must not vanish into the canvas"
                );
                assert_ne!(
                    Some(theme.background),
                    theme.code_inline.bg,
                    "{name:?}/{variant:?}: inline code needs an offset surface"
                );
                assert!(
                    !theme.default_syntax_theme().is_empty(),
                    "{name:?}/{variant:?}: every face pairs a syntect theme"
                );
                // Every heading level is bold; colour choice is exercised in
                // `heading_ladder_*`, independent of terminal colour depth.
                assert!(
                    theme
                        .headings
                        .iter()
                        .all(|h| h.add_modifier.contains(Modifier::BOLD)),
                    "{name:?}/{variant:?}: headings must be bold"
                );
            }
        }
    }
}
