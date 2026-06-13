//! Rendering for the editor: the hybrid focus-mode view (cursor block raw, rest
//! rendered) with a hardware cursor, the scroll-only full preview, and the help
//! overlay. The status bar shows the mode and the relevant hints.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout as RatatuiLayout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, LineNumbers};
use crate::color::rgb;

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
    if app.help_open {
        let lines = help_lines(app);
        // Clamp scrolling by *rendered* rows: on a narrow terminal a long help
        // line wraps onto several rows, so a plain `len - height` clamp would
        // leave the bottom lines unreachable. (This is the other half of the
        // contract in `App::scroll_help`, which leaves the upper bound to us.)
        let wrap_width = (content_area.width as usize).max(1);
        let mut rows = 0usize;
        let mut fit = 0usize;
        for line in lines.iter().rev() {
            rows += line_rows(line, wrap_width);
            if rows > height.max(1) {
                break;
            }
            fit += 1;
        }
        let max_scroll = lines.len().saturating_sub(fit.max(1));
        app.help_scroll = app.help_scroll.min(max_scroll);
        frame.render_widget(
            Paragraph::new(window(&lines, app.help_scroll, height))
                .style(Style::new().bg(app.theme().help_background))
                .wrap(Wrap { trim: false }),
            content_area,
        );
    } else if app.mode.is_read() {
        frame.render_widget(
            Paragraph::new(window(app.preview(), app.scroll_y, height)).style(app.theme().text),
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
        frame.render_widget(
            Paragraph::new(window(&app.view().lines, app.scroll_y, height)).style(app.theme().text),
            content_area,
        );
        if let Some(gutter_area) = gutter_area {
            frame.render_widget(
                Paragraph::new(gutter_lines(app, gutter_width, height)).style(app.theme().gutter),
                gutter_area,
            );
        }
        if prompt.is_none()
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
/// the space (`^G help · ^S save · ^Q quit`). A transient status message
/// temporarily replaces the right side. Everything else lives in `^G` help.
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
            let hints = if app.help_open {
                "^G/Esc close ".to_string()
            } else if app.mode.is_read() {
                "q/Esc back · ^G help · ^Q quit ".to_string()
            } else {
                let (line, col) = app.cursor_line_col();
                format!("Ln {line}, Col {col}   ^G help · ^S save · ^Q quit ")
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
    if app.help_open {
        return ("HELP", rgb(0x7d, 0xcf, 0xff));
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
    if app.help_open || app.mode.is_read() || app.line_numbers() == LineNumbers::Off {
        return 0;
    }
    let digits = app.buffer.rope().len_lines().max(1).to_string().len();
    (digits + 1).min(editor_width.saturating_sub(1))
}

fn gutter_lines(app: &App, width: usize, height: usize) -> Vec<Line<'static>> {
    let current = app.cursor_line_col().0;
    let mut out = Vec::with_capacity(height);
    for row in app.scroll_y..app.scroll_y + height {
        let num = app.view().line_numbers.get(row).and_then(|n| *n);
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

fn help_lines(app: &App) -> Vec<Line<'static>> {
    let title = Style::new()
        .fg(rgb(0x8b, 0xe9, 0xfd))
        .add_modifier(Modifier::BOLD);
    let heading = Style::new()
        .fg(rgb(0xff, 0xd8, 0x66))
        .add_modifier(Modifier::BOLD);
    let key = Style::new()
        .fg(rgb(0x50, 0xfa, 0x7b))
        .add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(rgb(0x9a, 0x9a, 0xb0));
    let text = Style::new().fg(rgb(0xe6, 0xe6, 0xf0));

    let mut lines = vec![
        Line::from(Span::styled("Marqi Help", title)),
        Line::from(vec![
            Span::styled("Preset: ", dim),
            Span::styled(app.preset_label().to_string(), key),
            Span::styled(
                "   Live preview opens only the block under the cursor.",
                text,
            ),
        ]),
        Line::default(),
        Line::from(Span::styled("Global", heading)),
        help_row(
            "^G",
            "toggle this help (scroll with j/k, arrows, PgUp/Dn)",
            key,
            text,
        ),
        help_row("^P", "toggle full rendered preview", key, text),
        help_row("^R", "toggle raw (highlighted source) view", key, text),
        help_row("^L", "cycle keybinding preset", key, text),
        help_row("^T", "toggle table row mode in pipe tables", key, text),
        help_row("^B", "toggle block / line cursor", key, text),
        help_row(
            "^S / ^Q",
            "save / quit (quit confirms when unsaved)",
            key,
            text,
        ),
        Line::default(),
        Line::from(Span::styled("Find & Replace", heading)),
        help_row(
            "^F",
            "find (also: / in Vim, ^W in Nano, M-s in Emacs)",
            key,
            text,
        ),
        help_row(
            "Enter / \u{2193}, \u{2191}",
            "next / previous match",
            key,
            text,
        ),
        help_row("Tab", "switch between find and replace", key, text),
        help_row(
            "Enter, ^A",
            "replace current match / replace all",
            key,
            text,
        ),
        help_row("n / N", "repeat the search (Vim normal mode)", key, text),
        Line::default(),
        Line::from(Span::styled("Mouse", heading)),
        help_row("click / drag", "place the cursor / select text", key, text),
        help_row(
            "wheel",
            "scroll freely (any key returns to the cursor)",
            key,
            text,
        ),
        Line::default(),
        Line::from(Span::styled("Standard", heading)),
        help_row("^A, ^C/^X/^V", "select all, copy/cut/paste", key, text),
        help_row("^Z, ^Y", "undo / redo", key, text),
        help_row("Ctrl+Left/Right", "move by word", key, text),
        help_row("Ctrl+Home/End", "document start / end", key, text),
        help_row(
            "Shift+movement",
            "select when your terminal reports enhanced keys",
            key,
            text,
        ),
        Line::default(),
        Line::from(Span::styled("Table Row Mode", heading)),
        help_row("Tab / Shift+Tab", "next / previous table cell", key, text),
        help_row("^N / ^D", "insert / delete data row", key, text),
        help_row("Alt+Up / Alt+Down", "move data row", key, text),
        Line::default(),
        Line::from(Span::styled("Vim", heading)),
        help_row(
            "i a I A o O",
            "insert, append, insert at line start/end, open lines",
            key,
            text,
        ),
        help_row(
            "h j k l / arrows",
            "move by grapheme and display row",
            key,
            text,
        ),
        help_row(
            "w b 0 $ gg G",
            "word, line, and document motions",
            key,
            text,
        ),
        help_row(
            "v, y, d, x, dd, yy, p",
            "visual select, yank, delete, cut, line ops, paste",
            key,
            text,
        ),
        help_row("u / ^R", "undo / redo", key, text),
        help_row("?", "toggle this help (normal mode)", key, text),
        Line::default(),
        Line::from(Span::styled("Nano", heading)),
        help_row("^K / ^U / ^O", "cut, paste, save", key, text),
        help_row(
            "Shift+movement",
            "select when your terminal reports enhanced keys",
            key,
            text,
        ),
        Line::default(),
        Line::from(Span::styled("Emacs", heading)),
        help_row(
            "C-a/e/f/n, arrows",
            "move (C-b/C-p are the global toggles above)",
            key,
            text,
        ),
        help_row("M-f / M-b", "move by word", key, text),
        help_row(
            "C-Space, C-w, M-w, C-y",
            "set mark, cut, copy, paste",
            key,
            text,
        ),
        help_row("C-x C-s / C-x C-c", "save / quit", key, text),
    ];

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Config: ~/.config/marqi/config.toml",
        dim,
    )));
    lines.push(Line::from(Span::styled(
        "       (macOS: ~/Library/Application Support/marqi/config.toml)",
        dim,
    )));
    lines
}

fn help_row(
    keys: &'static str,
    action: &'static str,
    key_style: Style,
    text_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {keys:<24}"), key_style),
        Span::styled(action, text_style),
    ])
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
        assert!(text.contains("^G help"));
        assert!(
            !text.contains("preview") && !text.contains("cursor:block"),
            "footer stays minimal: {text:?}"
        );
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
        let lines = gutter_lines(&app, width, 2);
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
