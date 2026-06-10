//! Terminal setup and teardown.
//!
//! [`init`] puts the terminal into raw mode on the alternate screen and installs
//! a panic hook so a crash never leaves the user's terminal in a broken state.
//! [`restore`] undoes it. The pair is deliberately explicit (rather than relying
//! on framework conveniences) so the "no leaked raw mode" guarantee is obvious.

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
};
use crossterm::{
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};

/// The concrete terminal type used throughout the app.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Enter raw mode + alternate screen and build the ratatui terminal.
///
/// The panic hook is installed before anything touches the terminal, and any
/// error after `enable_raw_mode` restores the terminal before propagating, so
/// no failure path can leave the user's shell in raw mode.
pub fn init() -> Result<Tui> {
    set_panic_hook();
    enable_raw_mode()?;
    init_after_raw_mode().inspect_err(|_| {
        let _ = restore();
    })
}

fn init_after_raw_mode() -> Result<Tui> {
    let mut stdout = io::stdout();
    // Mouse capture enables click-to-position, drag-select, and wheel scroll.
    // (Terminal-level text selection still works with Shift held.)
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Ask the terminal to report modified keys (Shift+Arrow, etc.) unambiguously
    // via the CSI-u protocol, where supported — needed for reliable selection.
    if matches!(supports_keyboard_enhancement(), Ok(true)) {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    Ok(terminal)
}

/// Leave the alternate screen and disable raw mode. Safe to call more than once.
pub fn restore() -> Result<()> {
    let mut stdout = io::stdout();
    // Pop the enhancement flags if we pushed them (ignored when unsupported).
    // This must happen *before* LeaveAlternateScreen: kitty keeps a separate
    // keyboard-flag stack per screen buffer, and the push happened on the
    // alternate screen.
    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    execute!(
        stdout,
        DisableMouseCapture,
        SetCursorStyle::DefaultUserShape,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    Ok(())
}

/// Chain a terminal-restoring step in front of the existing panic hook so the
/// terminal is cleaned up before the default panic message is printed.
fn set_panic_hook() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        hook(info);
    }));
}
