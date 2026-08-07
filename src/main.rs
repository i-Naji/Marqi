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
use std::io::{IsTerminal, Read, Write};
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
    marqi -r FILE             Render FILE (or - for stdin) to stdout

OPTIONS:
    -r, --render              Render instead of opening the editor
    --width COLUMNS           Set plain-text render width
    --html                    Render HTML instead of plain text
    -c, --config PATH         Load a config.toml (or DIR containing one)
    --                        Treat every following argument as a filename
    -h, --help                Show this help
    -V, --version             Show version

KEYS:
    ^S save · ^Q quit · ^F find · ^P preview · ^⇧P commands
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
        Action::Render(options) => {
            return render_to_stdout(&options, cli.config_path.as_deref());
        }
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
    if let Err(error) = app.load_session() {
        app.status = Some(format!("Session not restored: {error}"));
    }
    if let Some(warning) = config_warning {
        app.status = Some(warning);
    }
    app.offer_recovery();
    if app.prompt_view().is_none() && app.status.is_none() && config::take_first_run_hint() {
        app.status = Some(
            "Tip: move into a block to edit its Markdown · Ctrl+Shift+P opens commands".to_string(),
        );
    }

    let mut terminal = tui::init()?;
    #[cfg(debug_assertions)]
    if std::env::var_os("MARQI_TEST_PANIC_AFTER_INIT").is_some() {
        panic!("requested terminal cleanup probe");
    }
    let result = run(&mut terminal, &mut app);
    let session_error = app.save_session().err();
    // Restore the terminal even if the run loop errored. A run-loop error is
    // the more informative of the two, so report it first.
    let restored = tui::restore();
    if app::stats_enabled() {
        eprintln!("{}", app.stats_dump());
    }
    if let Some(error) = session_error {
        eprintln!("Session not saved: {error}");
    }
    result.and(restored)
}

/// Render markdown to stdout without requiring a TTY.
fn render_to_stdout(options: &RenderOptions, config_path: Option<&std::path::Path>) -> Result<()> {
    use markdown::{CodeHighlighter, MarkdownTheme, render_preview};

    let source = if options.path == "-" {
        let mut source = String::new();
        std::io::stdin().read_to_string(&mut source)?;
        source
    } else {
        std::fs::read_to_string(&options.path)
            .with_context(|| format!("reading {}", options.path))?
    };
    if options.html {
        let html = comrak::markdown_to_html(&source, &markdown::gfm_options());
        std::io::stdout().write_all(html.as_bytes())?;
        return Ok(());
    }
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
    let width = options.width.unwrap_or_else(|| {
        crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(80)
    });
    let mut output = std::io::BufWriter::new(std::io::stdout().lock());
    for line in render_preview(&source, width, &theme, &highlighter) {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        writeln!(output, "{text}")?;
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
    Render(RenderOptions),
}

struct RenderOptions {
    path: String,
    width: Option<usize>,
    html: bool,
}

impl Cli {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = args.into_iter();
        let mut config_path = None;
        let mut render = false;
        let mut render_width = None;
        let mut render_html = false;
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
                "--width" => {
                    let value = args
                        .next()
                        .context("usage: marqi -r FILE --width COLUMNS")?;
                    let width = value
                        .parse::<usize>()
                        .ok()
                        .filter(|width| *width > 0)
                        .context("render width must be a positive integer")?;
                    render_width = Some(width);
                }
                "--html" => render_html = true,
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
                "-" => positional(arg)?,
                _ if arg.starts_with('-') => anyhow::bail!("unknown option: {arg}\n\n{HELP}"),
                _ => positional(arg)?,
            }
        }

        let action = if render {
            Action::Render(RenderOptions {
                path: file.context("usage: marqi -r <file or ->")?,
                width: render_width,
                html: render_html,
            })
        } else {
            if render_width.is_some() || render_html {
                anyhow::bail!("--width and --html require --render");
            }
            if file.as_deref() == Some("-") {
                anyhow::bail!("- is only supported with --render");
            }
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
            Action::Render(options) => assert_eq!(options.path, "doc.md"),
            _ => panic!("expected render action"),
        }
    }

    #[test]
    fn parses_render_file_after_later_options() {
        let cli = parse(&["-r", "-c", "cfg", "doc.md"]);
        assert_eq!(cli.config_path, Some(PathBuf::from("cfg")));
        match cli.action {
            Action::Render(options) => assert_eq!(options.path, "doc.md"),
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
    fn parses_render_stdin_width_and_html() {
        let cli = parse(&["--render", "-", "--width", "42", "--html"]);
        match cli.action {
            Action::Render(options) => {
                assert_eq!(options.path, "-");
                assert_eq!(options.width, Some(42));
                assert!(options.html);
            }
            _ => panic!("expected render action"),
        }
    }

    #[test]
    fn render_options_require_render_mode_and_valid_width() {
        assert!(Cli::parse(["--width", "0", "doc.md"].map(String::from)).is_err());
        assert!(Cli::parse(["--html", "doc.md"].map(String::from)).is_err());
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
