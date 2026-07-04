//! Rendering for the editor: the hybrid focus-mode view (cursor block raw, rest
//! rendered) with a hardware cursor, the scroll-only full preview, and the
//! settings popup. The status bar shows the mode and the relevant hints.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout as RatatuiLayout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, LineNumbers, Preset};
use crate::color::rgb;

/// True on macOS, where keybinding hints use the ⌃/⌥/⇧ modifier glyphs instead
/// of the `Ctrl/Alt/Shift` words.
const IS_MAC: bool = cfg!(target_os = "macos");

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [editor_area, status_area] =
        RatatuiLayout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    // The left margin (config `editor.left_margin`) pads every view from the
    // terminal edge; clamp it so at least one content column survives. It is
    // painted with the theme background, which the terminal's own may differ
    // from.
    let margin = app
        .left_margin()
        .min(editor_area.width.saturating_sub(1) as usize) as u16;
    let editor_area = if margin > 0 {
        let [margin_area, rest] = RatatuiLayout::new(
            Direction::Horizontal,
            [Constraint::Length(margin), Constraint::Min(1)],
        )
        .areas(editor_area);
        frame.render_widget(
            Paragraph::new("").style(Style::new().bg(app.theme().background)),
            margin_area,
        );
        rest
    } else {
        editor_area
    };
    let gutter_width = gutter_width(app, editor_area.width as usize);
    let (gutter_area, content_area) = if gutter_width > 0 {
        let [gutter, content] = RatatuiLayout::new(
            Direction::Horizontal,
            [Constraint::Length(gutter_width as u16), Constraint::Min(1)],
        )
        .areas(editor_area);
        (Some(gutter), content)
    } else {
        (None, editor_area)
    };

    app.set_viewport(content_area.width as usize, content_area.height as usize);
    app.set_left_offset(margin as usize + gutter_width);

    let prompt = app.prompt_view();
    let height = content_area.height as usize;
    if app.mode.is_read() {
        frame.render_widget(
            Paragraph::new(app.visible_rows().lines).style(app.theme().text),
            content_area,
        );
    } else {
        // Follow the cursor unless the user wheel-scrolled away; then only
        // keep the free-scrolled viewport within bounds.
        if app.follows_cursor() {
            app.scroll_to_cursor();
        } else {
            app.clamp_scroll();
        }
        let assembled = app.visible_rows();
        frame.render_widget(
            Paragraph::new(assembled.lines).style(app.theme().text),
            content_area,
        );
        if let Some(gutter_area) = gutter_area {
            frame.render_widget(
                Paragraph::new(gutter_lines(app, &assembled.numbers, gutter_width, height))
                    .style(app.theme().gutter),
                gutter_area,
            );
        }
        // The hardware cursor is hidden while a prompt or the settings popup is
        // taking input (the popup shows a highlighted row instead).
        if prompt.is_none()
            && !app.menu_open()
            && let Some(pos) = cursor_position(app, content_area)
        {
            frame.set_cursor_position(pos);
        }
    }

    match prompt {
        Some(view) => {
            frame.render_widget(
                Paragraph::new(Line::from(view.text)).style(app.theme().status),
                status_area,
            );
            if let Some(col) = view.cursor_col {
                let x = status_area.x + col.min(status_area.width.saturating_sub(1));
                frame.set_cursor_position(Position::new(x, status_area.y));
            }
        }
        None => {
            frame.render_widget(
                Paragraph::new(build_status(app, status_area.width as usize))
                    .style(app.theme().status),
                status_area,
            );
        }
    }

    // The settings popup paints last, on top of the editor and status bar.
    if app.menu_open() {
        draw_menu(frame, app, editor_area);
    }
}

/// The slice of `lines` currently within the viewport.
fn window(lines: &[Line<'static>], scroll: usize, height: usize) -> Vec<Line<'static>> {
    let start = scroll.min(lines.len());
    let end = (start + height).min(lines.len());
    lines[start..end].to_vec()
}

/// Rendered rows a line occupies when word-wrapped to `width` (at least 1).
fn line_rows(line: &Line<'_>, width: usize) -> usize {
    let w: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
    w.div_ceil(width).max(1)
}

/// Screen position of the cursor in the hybrid view, or `None` when scrolled
/// out of view.
fn cursor_position(app: &App, area: Rect) -> Option<Position> {
    let (col, row) = app.cursor_screen();
    let y = row.checked_sub(app.scroll_y)?;
    if y >= area.height as usize {
        return None;
    }
    // At the end of an exactly-full row `col == area.width`; clamp so the
    // cursor stays inside the content area instead of spilling past its edge.
    let x = area.x + col.min(area.width.saturating_sub(1));
    Some(Position::new(x, area.y + y as u16))
}

/// The status bar: a colored mode badge, the file name (with a `[+]` modified
/// flag), and — right-aligned — the cursor position plus the three hints worth
/// the space (`^G menu · ^S save · ^Q quit`). A transient status message
/// temporarily replaces the right side. Everything else lives in the `^G` menu.
fn build_status(app: &App, width: usize) -> Line<'static> {
    let status = app.theme().status;
    let (label, accent) = mode_badge(app);
    let badge = format!(" {label} ");
    let badge_style = Style::new()
        .bg(accent)
        .fg(rgb(0x1a, 0x1b, 0x26))
        .add_modifier(Modifier::BOLD);
    let name = format!(" {}", app.buffer.file_name());
    let flag = if app.buffer.modified() { " [+]" } else { "" };

    let (right, right_style) = match &app.status {
        Some(message) => (format!("{message} "), status),
        None => {
            let hints = if app.menu_open() {
                "\u{2191}\u{2193} move · \u{2190}\u{2192} change · \u{23ce} accept · esc cancel "
                    .to_string()
            } else if app.mode.is_read() {
                "q/Esc back · ^G menu · ^Q quit ".to_string()
            } else {
                let (line, col) = app.cursor_line_col();
                format!("Ln {line}, Col {col}   ^G menu · ^S save · ^Q quit ")
            };
            (hints, status.add_modifier(Modifier::DIM))
        }
    };
    let right = if crate::app::stats_enabled() {
        format!("{} · {right}", app.stats_line())
    } else {
        right
    };

    let left_width = display_width(&badge) + display_width(&name) + display_width(flag);
    if left_width > width {
        let text = clip_to_width(&format!("{badge}{name}{flag}"), width);
        return Line::from(Span::styled(text, status));
    }

    let mut spans = vec![
        Span::styled(badge, badge_style),
        Span::styled(name, status),
        Span::styled(flag, status.fg(rgb(0xe0, 0xaf, 0x68))),
    ];
    let right_width = display_width(&right);
    if left_width + right_width <= width {
        spans.push(Span::styled(
            " ".repeat(width - left_width - right_width),
            status,
        ));
        spans.push(Span::styled(right, right_style));
    }
    Line::from(spans)
}

/// Badge label and accent color for the current mode.
fn mode_badge(app: &App) -> (&'static str, ratatui::style::Color) {
    if app.menu_open() {
        return ("MENU", rgb(0x7d, 0xcf, 0xff));
    }
    if app.mode.is_read() {
        return ("READ", rgb(0x7d, 0xcf, 0xff));
    }
    if app.raw_view() {
        return ("RAW", rgb(0xf7, 0x76, 0x8e));
    }
    if app.table_mode() {
        return ("TABLE", rgb(0xe0, 0xaf, 0x68));
    }
    match app.mode {
        crate::app::Mode::Insert => ("INSERT", rgb(0x7a, 0xa2, 0xf7)),
        crate::app::Mode::Normal => ("NORMAL", rgb(0x9e, 0xce, 0x6a)),
        crate::app::Mode::Visual => ("VISUAL", rgb(0xbb, 0x9a, 0xf7)),
        crate::app::Mode::Read => ("READ", rgb(0x7d, 0xcf, 0xff)),
    }
}

fn gutter_width(app: &App, editor_width: usize) -> usize {
    // The settings popup floats over the editor, so the line-number gutter
    // stays (unlike read mode, which replaces the editor entirely).
    if app.mode.is_read() || app.line_numbers() == LineNumbers::Off {
        return 0;
    }
    let digits = app.buffer.rope().len_lines().max(1).to_string().len();
    (digits + 1).min(editor_width.saturating_sub(1))
}

fn gutter_lines(
    app: &App,
    numbers: &[Option<usize>],
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let current = app.cursor_line_col().0;
    let mut out = Vec::with_capacity(height);
    for row in 0..height {
        let num = numbers.get(row).copied().flatten();
        let text = num
            .map(|line| match app.line_numbers() {
                LineNumbers::Absolute => line,
                LineNumbers::Relative if line == current => line,
                LineNumbers::Relative => line.abs_diff(current),
                LineNumbers::Off => 0,
            })
            .filter(|n| *n > 0)
            .map(|n| format!("{n:>w$} ", w = width.saturating_sub(1)))
            .unwrap_or_else(|| " ".repeat(width));
        let style = if num == Some(current) {
            app.theme().gutter_current
        } else {
            app.theme().gutter
        };
        out.push(Line::from(Span::styled(text, style)));
    }
    out
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn clip_to_width(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for g in text.graphemes(true) {
        let w = display_width(g);
        if used + w > width {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out
}

/// Platform-aware Control chord: `⌃S` on macOS, `Ctrl+S` elsewhere.
fn ck(k: &str) -> String {
    if IS_MAC {
        format!("\u{2303}{k}")
    } else {
        format!("Ctrl+{k}")
    }
}

/// Platform-aware Shift chord: `⇧` on macOS, `Shift+` elsewhere.
fn sk(k: &str) -> String {
    if IS_MAC {
        format!("\u{21e7}{k}")
    } else {
        format!("Shift+{k}")
    }
}

/// One guide row: a left-aligned key column plus its action.
fn guide_row(keys: String, action: &str, key_style: Style, text_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {keys:<18}"), key_style),
        Span::styled(format!(" {action}"), text_style),
    ])
}

/// The keybinding guide, formatted for the current platform and filtered to
/// the active preset (Vim letter-keys and Emacs `C-`/`M-` notation are mode
/// notation, kept verbatim; the Ctrl-based chords adapt to the platform).
fn guide_lines(app: &App) -> Vec<Line<'static>> {
    let theme = app.theme();
    let heading = theme.heading(2);
    let key = theme.list_marker;
    let text = theme.text;
    let plat = if IS_MAC { "macOS" } else { "Linux / Windows" };

    let mut out = vec![
        Line::from(Span::styled(format!("Global \u{b7} {plat}"), heading)),
        guide_row(
            format!("{} / {}", ck("S"), ck("Q")),
            "save \u{b7} quit",
            key,
            text,
        ),
        guide_row(ck("P"), "toggle preview", key, text),
        guide_row(ck("R"), "raw source view", key, text),
        guide_row(ck("L"), "cycle keybindings", key, text),
        guide_row(ck("T"), "table row mode", key, text),
        guide_row(ck("B"), "block / line cursor", key, text),
        Line::default(),
        Line::from(Span::styled("Find & Replace", heading)),
        guide_row(
            "\u{21b5} / \u{2193} \u{2191}".to_string(),
            "next \u{b7} previous match",
            key,
            text,
        ),
        guide_row("Tab".to_string(), "switch find / replace", key, text),
        guide_row(
            format!("\u{21b5} \u{b7} {}", ck("A")),
            "replace one \u{b7} replace all",
            key,
            text,
        ),
        Line::default(),
        Line::from(Span::styled("Mouse", heading)),
        guide_row(
            "click / drag".to_string(),
            "place cursor \u{b7} select",
            key,
            text,
        ),
        guide_row("wheel".to_string(), "scroll (any key returns)", key, text),
        Line::default(),
    ];

    match app.preset() {
        Preset::Standard => {
            out.push(Line::from(Span::styled("Standard", heading)));
            out.push(guide_row(
                format!("{} {}/{}/{}", ck("A"), ck("C"), ck("X"), ck("V")),
                "select all \u{b7} copy/cut/paste",
                key,
                text,
            ));
            out.push(guide_row(
                format!("{} / {}", ck("Z"), ck("Y")),
                "undo \u{b7} redo",
                key,
                text,
            ));
            out.push(guide_row(ck("F"), "find", key, text));
            out.push(guide_row(
                ck("\u{2190}/\u{2192}"),
                "move by word",
                key,
                text,
            ));
            out.push(guide_row(
                ck("Home/End"),
                "document start \u{b7} end",
                key,
                text,
            ));
            out.push(guide_row(sk("motion"), "extend selection", key, text));
        }
        Preset::Vim => {
            out.push(Line::from(Span::styled("Vim", heading)));
            out.push(guide_row(
                "i a I A o O".to_string(),
                "insert \u{b7} append \u{b7} open lines",
                key,
                text,
            ));
            out.push(guide_row(
                "h j k l".to_string(),
                "move (also arrows)",
                key,
                text,
            ));
            out.push(guide_row(
                "w b 0 $ gg G".to_string(),
                "word \u{b7} line \u{b7} document motions",
                key,
                text,
            ));
            out.push(guide_row(
                "v y d x dd yy p".to_string(),
                "visual \u{b7} yank \u{b7} delete \u{b7} paste",
                key,
                text,
            ));
            out.push(guide_row(
                "/ \u{b7} n / N".to_string(),
                "find \u{b7} next / previous",
                key,
                text,
            ));
            out.push(guide_row(
                "u \u{b7} ^R".to_string(),
                "undo \u{b7} redo",
                key,
                text,
            ));
            out.push(guide_row(
                "?".to_string(),
                "this menu (normal mode)",
                key,
                text,
            ));
        }
        Preset::Nano => {
            out.push(Line::from(Span::styled("Nano", heading)));
            out.push(guide_row(
                format!("{} / {} / {}", ck("K"), ck("U"), ck("O")),
                "cut \u{b7} paste \u{b7} save",
                key,
                text,
            ));
            out.push(guide_row(
                format!("{} / {}", ck("F"), ck("W")),
                "find (\"Where Is\")",
                key,
                text,
            ));
            out.push(guide_row(sk("motion"), "extend selection", key, text));
        }
        Preset::Emacs => {
            out.push(Line::from(Span::styled("Emacs", heading)));
            out.push(guide_row(
                "C-a/e/f/n".to_string(),
                "line start/end \u{b7} char \u{b7} line (arrows too)",
                key,
                text,
            ));
            out.push(guide_row(
                "M-f / M-b".to_string(),
                "move by word",
                key,
                text,
            ));
            out.push(guide_row("M-s".to_string(), "find", key, text));
            out.push(guide_row(
                "C-Space C-w M-w C-y".to_string(),
                "mark \u{b7} cut \u{b7} copy \u{b7} paste",
                key,
                text,
            ));
            out.push(guide_row(
                "C-x C-s / C-x C-c".to_string(),
                "save \u{b7} quit",
                key,
                text,
            ));
        }
    }
    out
}

/// The three focusable settings rows (keybindings, theme, appearance) with the
/// focused row's value shown between `\u{25c2} \u{25b8}` selectors.
fn setting_rows(app: &App) -> Vec<Line<'static>> {
    let theme = app.theme();
    let focus = app.menu_focus();
    let label = theme.marker;
    let text = theme.text;
    let accent = theme.keyword_note;
    let arrow = theme.list_marker;

    let kb = match app.preset() {
        Preset::Standard => "Standard",
        Preset::Vim => "Vim",
        Preset::Nano => "Nano",
        Preset::Emacs => "Emacs",
    };
    let ln = match app.line_numbers() {
        LineNumbers::Off => "off",
        LineNumbers::Absolute => "absolute",
        LineNumbers::Relative => "relative",
    };
    let rows: [(&str, &str); 4] = [
        ("Keybindings", kb),
        ("Theme", theme.name.label()),
        ("Appearance", theme.variant.label()),
        ("Line numbers", ln),
    ];
    rows.iter()
        .enumerate()
        .map(|(i, (name, val))| {
            let focused = i == focus;
            let (mark, lhs, rhs, vstyle) = if focused {
                ("\u{25b8} ", "\u{25c2} ", " \u{25b8}", accent)
            } else {
                ("  ", "  ", "  ", text)
            };
            Line::from(vec![
                Span::styled(format!("  {mark}"), arrow),
                Span::styled(format!("{name:<13}"), label),
                Span::styled(lhs.to_string(), arrow),
                Span::styled(val.to_string(), vstyle),
                Span::styled(rhs.to_string(), arrow),
            ])
        })
        .collect()
}

/// Draw the centered Settings popup over `area`: rounded border, the three
/// setting rows pinned at the top, and the scrollable keybinding guide below.
fn draw_menu(frame: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let settings = setting_rows(app);
    let guide = guide_lines(app);

    let max_w = area.width.saturating_sub(2).max(1);
    let popup_w = 72u16.min(max_w);
    let want_h = settings.len() as u16 + 1 + guide.len() as u16 + 2;
    let max_h = area.height.saturating_sub(1).max(1);
    let popup_h = want_h.min(max_h);
    let px = area.x + area.width.saturating_sub(popup_w) / 2;
    let py = area.y + area.height.saturating_sub(popup_h) / 2;
    let popup = Rect::new(px, py, popup_w, popup_h);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Span::styled(" Settings ", theme.heading(2)))
        .style(theme.text.bg(theme.help_background));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Settings rows pinned at the top; the guide scrolls in the space below.
    let top_h = (settings.len() as u16 + 1).min(inner.height);
    let [top, bottom] =
        RatatuiLayout::vertical([Constraint::Length(top_h), Constraint::Min(0)]).areas(inner);

    let mut top_lines = settings;
    top_lines.push(Line::default());
    frame.render_widget(Paragraph::new(top_lines).style(theme.text), top);

    // Clamp the guide scroll so the last rows stay reachable (rendered-row
    // aware, like the former help overlay).
    let gh = bottom.height as usize;
    let wrap_w = (bottom.width as usize).max(1);
    let mut rows = 0usize;
    let mut fit = 0usize;
    for line in guide.iter().rev() {
        rows += line_rows(line, wrap_w);
        if rows > gh.max(1) {
            break;
        }
        fit += 1;
    }
    let max_scroll = guide.len().saturating_sub(fit.max(1));
    let scroll = app.menu_guide_scroll().min(max_scroll);
    frame.render_widget(
        Paragraph::new(window(&guide, scroll, gh))
            .style(theme.text)
            .wrap(Wrap { trim: false }),
        bottom,
    );
}

#[cfg(test)]
mod tests {
    use super::{build_status, clip_to_width, display_width, gutter_lines, gutter_width};
    use crate::{app::App, buffer::TextBuffer, config::Config};

    #[test]
    fn clipping_preserves_whole_graphemes() {
        assert_eq!(clip_to_width("a中b", 3), "a中");
        assert_eq!(clip_to_width("👨‍👩‍👧‍👦x", 2), "👨‍👩‍👧‍👦");
    }

    #[test]
    fn status_bar_fills_width_with_badge_position_and_hints() {
        let mut app = App::with_config(TextBuffer::scratch("hi\n", "test.md"), &Config::default());
        app.set_viewport(80, 10);
        app.scroll_to_cursor();
        let line = build_status(&app, 80);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(display_width(&text), 80, "status bar spans the full width");
        assert!(text.starts_with(" INSERT "), "mode badge leads: {text:?}");
        assert!(text.contains("test.md"));
        assert!(text.contains("Ln 1, Col 1"));
        assert!(text.contains("^G menu"));
        assert!(
            !text.contains("preview") && !text.contains("cursor:block"),
            "footer stays minimal: {text:?}"
        );
    }

    #[test]
    fn guide_is_filtered_to_the_active_preset() {
        use super::guide_lines;

        let flat = |app: &App| -> String {
            guide_lines(app)
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| s.content.to_string())
                .collect()
        };

        let vim_cfg: Config = toml::from_str("[editor]\nkeybindings = \"vim\"\n").unwrap();
        let vim = App::with_config(TextBuffer::scratch("x\n", "t.md"), &vim_cfg);
        let text = flat(&vim);
        assert!(text.contains("Vim"), "Vim section present: {text}");
        assert!(!text.contains("Emacs"), "other presets hidden: {text}");
        assert!(!text.contains("Nano"));

        let std_app = App::with_config(TextBuffer::scratch("x\n", "t.md"), &Config::default());
        let text = flat(&std_app);
        assert!(text.contains("Standard"));
        assert!(!text.contains("Vim"));
    }

    #[test]
    fn narrow_status_bar_keeps_the_left_side() {
        let mut app = App::with_config(TextBuffer::scratch("hi\n", "test.md"), &Config::default());
        app.set_viewport(20, 10);
        let line = build_status(&app, 20);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("test.md"), "filename survives narrowing");
        assert!(display_width(&text) <= 20);
    }

    #[test]
    fn gutter_formats_absolute_line_numbers() {
        let cfg: Config = toml::from_str("[editor]\nline_numbers = \"absolute\"\n").unwrap();
        let mut app = App::with_config(TextBuffer::scratch("a\nb\n", "test.md"), &cfg);
        app.set_viewport(20, 4);
        app.scroll_to_cursor();

        let width = gutter_width(&app, 20);
        assert_eq!(width, 2);
        let assembled = app.visible_rows();
        let lines = gutter_lines(&app, &assembled.numbers, width, 2);
        let first: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(first, "1 ");
    }

    #[test]
    fn gutter_stays_off_by_default() {
        let mut app = App::with_config(TextBuffer::scratch("a\n", "test.md"), &Config::default());
        app.set_viewport(20, 4);
        assert_eq!(gutter_width(&app, 20), 0);
    }

    /// Render a full frame and return the symbols of the first row.
    fn first_row(app: &mut App, width: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| super::draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..width)
            .map(|x| buffer.cell((x, 0)).unwrap().symbol())
            .collect()
    }

    /// Opening the settings popup over double-width text (CJK, emoji) must
    /// repaint the wide glyphs' trailing cells. ratatui-core 0.1.1's buffer
    /// diff skipped them when the popup's blank cells carried the same style
    /// as the covered text, leaving half-glyph debris inside the popup on real
    /// terminals (menu "offset" over CJK/emoji lines). Fixed by ratatui-core
    /// 0.1.2; this pins the behaviour by checking the backend grid — the
    /// diff-applied view a terminal would show — against the composed frame.
    #[test]
    fn menu_repaints_trailing_cells_of_wide_chars() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let doc = "# Title\n\nEmphasis around CJK text: **中文加粗** and *日本語斜体*.\n\n- list item\n";
        let mut app = App::with_config(TextBuffer::scratch(doc, "test.md"), &Config::default());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| super::draw(frame, &mut app)).unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(app.menu_open());
        let mut composed: Option<ratatui::buffer::Buffer> = None;
        terminal
            .draw(|frame| {
                super::draw(frame, &mut app);
                composed = Some(frame.buffer_mut().clone());
            })
            .unwrap();
        let composed = composed.unwrap();

        // Every cell the frame composed must reach the terminal, except cells
        // hidden under a wide glyph (their content is invisible by contract).
        let shown = terminal.backend().buffer();
        for y in 0..24u16 {
            let mut hidden = 0usize;
            for x in 0..80u16 {
                let want = composed.cell((x, y)).unwrap();
                if hidden > 0 {
                    hidden -= 1;
                    continue;
                }
                hidden = display_width(want.symbol()).saturating_sub(1);
                let got = shown.cell((x, y)).unwrap();
                assert_eq!(
                    got, want,
                    "cell ({x},{y}) on the terminal differs from the composed frame"
                );
            }
        }
    }

    #[test]
    fn default_left_margin_pads_one_column() {
        let mut app = App::with_config(TextBuffer::scratch("hi\n", "test.md"), &Config::default());
        assert!(first_row(&mut app, 20).starts_with(" hi"));
    }

    #[test]
    fn left_margin_zero_starts_at_the_edge() {
        let cfg: Config = toml::from_str("[editor]\nleft_margin = 0\n").unwrap();
        let mut app = App::with_config(TextBuffer::scratch("hi\n", "test.md"), &cfg);
        assert!(first_row(&mut app, 20).starts_with("hi"));
    }

    #[test]
    fn left_margin_shifts_content_and_gutter() {
        let cfg: Config =
            toml::from_str("[editor]\nleft_margin = 3\nline_numbers = \"absolute\"\n").unwrap();
        let mut app = App::with_config(TextBuffer::scratch("hi\n", "test.md"), &cfg);
        assert!(first_row(&mut app, 20).starts_with("   1 hi"));
    }

    #[test]
    fn oversized_left_margin_keeps_a_content_column() {
        let cfg: Config = toml::from_str("[editor]\nleft_margin = 100\n").unwrap();
        let mut app = App::with_config(TextBuffer::scratch("hi\n", "test.md"), &cfg);
        // 9 margin columns (width - 1) and a single content column remain.
        assert_eq!(first_row(&mut app, 10), "         h");
    }
}
