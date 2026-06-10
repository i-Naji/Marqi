//! Clipboard access: the system clipboard via `arboard`, an internal register
//! as an always-available fallback, and OSC 52 emission so copy works over SSH
//! when no native clipboard is reachable.

use std::io::Write;

pub struct Clipboard {
    system: Option<arboard::Clipboard>,
    register: String,
    /// Emit OSC 52 when the system clipboard is missing or fails (e.g. over
    /// SSH, or a transient platform-clipboard error). Disabled only in tests.
    allow_osc52: bool,
}

impl Clipboard {
    pub fn new() -> Self {
        Self {
            system: arboard::Clipboard::new().ok(),
            register: String::new(),
            allow_osc52: true,
        }
    }

    /// A clipboard that uses only the internal register — no system clipboard
    /// and no OSC 52. Used in tests so they neither clobber the real clipboard
    /// nor write escape sequences to stdout.
    #[cfg(test)]
    pub fn internal_only() -> Self {
        Self {
            system: None,
            register: String::new(),
            allow_osc52: false,
        }
    }

    /// Copy `text` to the clipboard: the internal register always, the system
    /// clipboard when available, and OSC 52 whenever the system clipboard did
    /// not take the copy (remote sessions, transient clipboard errors).
    ///
    /// On Linux, OSC 52 is emitted *in addition* to the system clipboard: X11
    /// and Wayland clipboards are owned by the running process, so a copy
    /// would vanish when marqi exits (unless a clipboard manager is running).
    /// Handing the text to the terminal as well makes it outlive the process.
    pub fn set(&mut self, text: &str) {
        self.register = text.to_string();
        let copied = self
            .system
            .as_mut()
            .is_some_and(|cb| cb.set_text(text).is_ok());
        let linux = cfg!(all(unix, not(target_os = "macos")));
        if (!copied || linux) && self.allow_osc52 {
            emit_osc52(text);
        }
    }

    /// Read the clipboard, preferring the system clipboard and falling back to
    /// the internal register.
    pub fn get(&mut self) -> String {
        if let Some(cb) = &mut self.system
            && let Ok(text) = cb.get_text()
        {
            return text;
        }
        self.register.clone()
    }
}

/// Write an OSC 52 clipboard-set escape sequence to the terminal.
fn emit_osc52(text: &str) {
    // Terminals cap OSC 52 payloads (xterm: ~100 KB) and silently drop larger
    // sequences, so don't emit one that cannot arrive. The internal register
    // still holds the full text for in-app paste.
    const MAX_PAYLOAD: usize = 99_000;
    let payload = base64_encode(text.as_bytes());
    if payload.len() > MAX_PAYLOAD {
        return;
    }
    let mut seq = format!("\x1b]52;c;{payload}\x07");
    // tmux < 3.3 swallows OSC 52 unless it is wrapped in a DCS passthrough
    // (with every ESC in the payload doubled).
    if std::env::var_os("TMUX").is_some() {
        seq = format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"));
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

/// Minimal standard base64 encoder (no external dependency).
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(TABLE[(n >> 18 & 63) as usize] as char);
        out.push(TABLE[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }
}
