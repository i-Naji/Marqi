//! Marqi — a live-preview TUI markdown editor.
//!
//! Entry point: parse args (or pipe markdown on stdin), set up the terminal,
//! run the event loop, and always restore the terminal afterwards.

mod app;
mod block_index;
mod buffer;
mod clipboard;
mod color;
mod config;
mod cursor;
mod history;
mod layout;
mod line_index;
mod markdown;
#[cfg(test)]
mod testdoc;
mod text;
mod tui;
mod ui;
mod view;

use std::env;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::{
    cursor::SetCursorStyle,
    event::{self, Event, KeyEventKind},
    execute,
};

use app::{App, CursorShape};
use buffer::TextBuffer;

const HELP: &str = "\
marqi — a live-preview terminal markdown editor

USAGE:
    marqi [OPTIONS] [FILE]    Open FILE (or a new buffer if omitted)
    curl -s URL | marqi       Edit markdown piped on stdin
    marqi -r FILE             Render FILE to stdout

OPTIONS:
    -r, --render              Render instead of opening the editor
    -c, --config PATH         Load a config.toml (or DIR containing one)
    --                        Treat every following argument as a filename
    -h, --help                Show this help
    -V, --version             Show version

KEYS:
    ^S save · ^Q quit · ^F find · ^P preview · ^L switch preset
    Press ^G inside the editor for the full, scrollable reference.

CONFIG (optional): ~/.config/marqi/config.toml — see marqi.example.toml
";

fn main() -> Result<()> {
    let cli = Cli::parse(env::args().skip(1))?;

    match cli.action {
        Action::Help => {
            print!("{HELP}");
            return Ok(());
        }
        Action::Version => {
            println!("marqi {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Action::Render(path) => return render_to_stdout(&path, cli.config_path.as_deref()),
        Action::Edit(path) => run_editor(path, cli.config_path)?,
    }

    Ok(())
}

fn run_editor(path: Option<String>, config_path: Option<PathBuf>) -> Result<()> {
    // The TUI draws to stdout; with stdout redirected the user would see a
    // frozen terminal stuck in raw mode while frames fill the file.
    if !std::io::stdout().is_terminal() {
        anyhow::bail!("stdout is not a terminal; use `marqi -r FILE` to render to a file or pipe");
    }

    // A positional (non-flag) argument is a file; otherwise read piped stdin, or
    // start empty when stdin is an interactive terminal.
    let buffer = match path {
        Some(path) => TextBuffer::from_path(path)?,
        None if std::io::stdin().is_terminal() => TextBuffer::empty(),
        None => {
            let mut content = String::new();
            std::io::stdin().read_to_string(&mut content)?;
            TextBuffer::scratch(&content, "[stdin]")
        }
    };

    let (config, config_warning) = load_config(config_path.as_deref());
    let mut app = App::with_config(buffer, &config);
    if let Some(warning) = config_warning {
        app.status = Some(warning);
    }
    app.offer_recovery();

    let mut terminal = tui::init()?;
    let result = run(&mut terminal, &mut app);
    // Restore the terminal even if the run loop errored. A run-loop error is
    // the more informative of the two, so report it first.
    let restored = tui::restore();
    if app::stats_enabled() {
        eprintln!("{}", app.stats_dump());
    }
    result.and(restored)
}

/// Render a file's markdown preview to stdout as plain text (no TTY needed).
fn render_to_stdout(path: &str, config_path: Option<&std::path::Path>) -> Result<()> {
    use markdown::{CodeHighlighter, MarkdownTheme, render_preview};

    let source = std::fs::read_to_string(path)?;
    let (cfg, warning) = load_config(config_path);
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    let mut theme = MarkdownTheme::select(&cfg.theme.name, &cfg.theme.variant);
    theme.heading_glyphs = cfg.editor.heading_glyphs;
    theme.hard_breaks = cfg.editor.soft_break.trim().eq_ignore_ascii_case("break");
    theme.apply_overrides(&cfg.theme.markdown);
    let syntax = cfg
        .theme
        .syntax
        .as_deref()
        .unwrap_or_else(|| theme.default_syntax_theme());
    let highlighter = CodeHighlighter::new(Some(syntax));
    let width = crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80);
    for line in render_preview(&source, width, &theme, &highlighter) {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        println!("{text}");
    }
    Ok(())
}

fn load_config(config_path: Option<&std::path::Path>) -> (config::Config, Option<String>) {
    match config_path {
        Some(arg) => config::Config::load_from_arg_with_warning(arg),
        None => config::Config::load_with_warning(),
    }
}

struct Cli {
    action: Action,
    config_path: Option<PathBuf>,
}

enum Action {
    Help,
    Version,
    Edit(Option<String>),
    Render(String),
}

impl Cli {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = args.into_iter();
        let mut config_path = None;
        let mut render = false;
        let mut file = None;

        let mut positional = |arg: String| -> Result<()> {
            if file.replace(arg).is_some() {
                anyhow::bail!("unexpected extra argument\n\n{HELP}");
            }
            Ok(())
        };

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => {
                    return Ok(Self {
                        action: Action::Help,
                        config_path,
                    });
                }
                "-V" | "--version" => {
                    return Ok(Self {
                        action: Action::Version,
                        config_path,
                    });
                }
                "-r" | "--render" => render = true,
                "-c" | "--config" => {
                    let path = args
                        .next()
                        .filter(|path| !path.starts_with('-'))
                        .context("usage: marqi -c <config.toml or dir> [FILE]")?;
                    config_path = Some(PathBuf::from(path));
                }
                // Everything after `--` is a filename, even if it looks like a flag.
                "--" => {
                    for rest in args.by_ref() {
                        positional(rest)?;
                    }
                }
                _ if arg.starts_with('-') => anyhow::bail!("unknown option: {arg}\n\n{HELP}"),
                _ => positional(arg)?,
            }
        }

        let action = if render {
            Action::Render(file.context("usage: marqi -r <file>")?)
        } else {
            Action::Edit(file)
        };

        Ok(Self {
            action,
            config_path,
        })
    }
}

/// Render-on-event loop: draw, wait for the next event, react, repeat.
fn run(terminal: &mut tui::Tui, app: &mut App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
        apply_cursor_shape(terminal, app)?;
        // Poll with a short timeout only while an auto-save may be pending;
        // otherwise wait long (redrawing on timeout is a no-op diff). A
        // `Resize` simply falls through and redraws, since `Terminal::draw`
        // re-queries the terminal size each frame.
        let timeout = if app.wants_tick() {
            std::time::Duration::from_millis(500)
        } else {
            std::time::Duration::from_secs(60)
        };
        if !event::poll(timeout)? {
            app.tick();
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
            Event::Mouse(mouse) => app.handle_mouse(mouse),
            _ => {}
        }
    }
    Ok(())
}

fn apply_cursor_shape(terminal: &mut tui::Tui, app: &App) -> Result<()> {
    let style = match app.cursor_shape() {
        CursorShape::Block => SetCursorStyle::SteadyBlock,
        CursorShape::Line => SetCursorStyle::SteadyBar,
    };
    execute!(terminal.backend_mut(), style)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::parse(args.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn parses_render_alias_and_config_path() {
        let cli = parse(&["-c", "cfg", "-r", "doc.md"]);
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg")));
        match cli.action {
            Action::Render(path) => assert_eq!(path, "doc.md"),
            _ => panic!("expected render action"),
        }
    }

    #[test]
    fn parses_render_file_after_later_options() {
        let cli = parse(&["-r", "-c", "cfg", "doc.md"]);
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg")));
        match cli.action {
            Action::Render(path) => assert_eq!(path, "doc.md"),
            _ => panic!("expected render action"),
        }
    }

    #[test]
    fn parses_edit_file_with_config_path() {
        let cli = parse(&["--config", "cfg", "doc.md"]);
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg")));
        match cli.action {
            Action::Edit(path) => assert_eq!(path.as_deref(), Some("doc.md")),
            _ => panic!("expected edit action"),
        }
    }

    #[test]
    fn missing_render_file_is_an_error() {
        assert!(Cli::parse(["-r".to_string()]).is_err());
    }

    #[test]
    fn config_value_must_not_look_like_a_flag() {
        // `marqi -c -r doc.md` is a mistake, not a config dir named "-r".
        assert!(Cli::parse(["-c", "-r", "doc.md"].map(String::from)).is_err());
    }

    #[test]
    fn extra_render_file_is_an_error() {
        assert!(Cli::parse(["-r", "a.md", "b.md"].map(String::from)).is_err());
    }

    #[test]
    fn double_dash_allows_dash_prefixed_filenames() {
        let cli = parse(&["--", "-r"]);
        match cli.action {
            Action::Edit(path) => assert_eq!(path.as_deref(), Some("-r")),
            _ => panic!("expected edit action"),
        }
    }
}
