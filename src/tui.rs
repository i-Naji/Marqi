//! Terminal setup and teardown.
//!
//! [`init`] puts the terminal into raw mode on the alternate screen and installs
//! a panic hook so a crash never leaves the user's terminal in a broken state.
//! [`restore`] undoes it. The pair is deliberately explicit (rather than relying
//! on framework conveniences) so the "no leaked raw mode" guarantee is obvious.

use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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

static KEYBOARD_FLAGS_PUSHED: AtomicBool = AtomicBool::new(false);

fn init_after_raw_mode() -> Result<Tui> {
    let mut stdout = io::stdout();
    // Mouse capture enables click-to-position, drag-select, and wheel scroll.
    // (Terminal-level text selection still works with Shift held.)
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    // Ask the terminal to report modified keys (Shift+Arrow, etc.) unambiguously
    // via the CSI-u protocol, where supported — needed for reliable selection.
    if matches!(supports_keyboard_enhancement(), Ok(true))
        && execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
    {
        KEYBOARD_FLAGS_PUSHED.store(true, Ordering::Relaxed);
    }
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    Ok(terminal)
}

/// Leave the alternate screen and disable raw mode. Safe to call more than once.
pub fn restore() -> Result<()> {
    restore_with(&mut TerminalRestore {
        stdout: io::stdout(),
    })
}

trait RestoreOps {
    fn pop_keyboard_flags(&mut self);
    fn disable_mouse(&mut self) -> io::Result<()>;
    fn disable_paste(&mut self) -> io::Result<()>;
    fn reset_cursor(&mut self) -> io::Result<()>;
    fn leave_screen(&mut self) -> io::Result<()>;
    fn disable_raw(&mut self) -> io::Result<()>;
}

struct TerminalRestore {
    stdout: Stdout,
}

impl RestoreOps for TerminalRestore {
    fn pop_keyboard_flags(&mut self) {
        if KEYBOARD_FLAGS_PUSHED.swap(false, Ordering::Relaxed) {
            let _ = execute!(self.stdout, PopKeyboardEnhancementFlags);
        }
    }

    fn disable_mouse(&mut self) -> io::Result<()> {
        execute!(self.stdout, DisableMouseCapture)
    }

    fn disable_paste(&mut self) -> io::Result<()> {
        execute!(self.stdout, DisableBracketedPaste)
    }

    fn reset_cursor(&mut self) -> io::Result<()> {
        execute!(self.stdout, SetCursorStyle::DefaultUserShape)
    }

    fn leave_screen(&mut self) -> io::Result<()> {
        execute!(self.stdout, LeaveAlternateScreen)
    }

    fn disable_raw(&mut self) -> io::Result<()> {
        disable_raw_mode()
    }
}

fn restore_with(ops: &mut impl RestoreOps) -> Result<()> {
    let mut first_error = None;
    ops.pop_keyboard_flags();
    remember_error(&mut first_error, ops.disable_mouse());
    remember_error(&mut first_error, ops.disable_paste());
    remember_error(&mut first_error, ops.reset_cursor());
    remember_error(&mut first_error, ops.leave_screen());
    remember_error(&mut first_error, ops.disable_raw());
    first_error.map_or(Ok(()), Err)
}

fn remember_error(first: &mut Option<anyhow::Error>, result: io::Result<()>) {
    if let Err(error) = result
        && first.is_none()
    {
        *first = Some(error.into());
    }
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

#[cfg(test)]
mod tests {
    use super::{RestoreOps, restore_with};
    use std::io;

    #[derive(Default)]
    struct FailingRestore {
        steps: Vec<&'static str>,
    }

    impl RestoreOps for FailingRestore {
        fn pop_keyboard_flags(&mut self) {
            self.steps.push("keyboard");
        }

        fn disable_mouse(&mut self) -> io::Result<()> {
            self.steps.push("mouse");
            Err(io::Error::other("mouse failed"))
        }

        fn disable_paste(&mut self) -> io::Result<()> {
            self.steps.push("paste");
            Ok(())
        }

        fn reset_cursor(&mut self) -> io::Result<()> {
            self.steps.push("cursor");
            Ok(())
        }

        fn leave_screen(&mut self) -> io::Result<()> {
            self.steps.push("screen");
            Err(io::Error::other("screen failed"))
        }

        fn disable_raw(&mut self) -> io::Result<()> {
            self.steps.push("raw");
            Ok(())
        }
    }

    #[test]
    fn restoration_continues_after_an_error() {
        let mut ops = FailingRestore::default();
        let error = restore_with(&mut ops).unwrap_err();

        assert_eq!(
            ops.steps,
            ["keyboard", "mouse", "paste", "cursor", "screen", "raw"]
        );
        assert!(error.to_string().contains("mouse failed"));
    }
}
