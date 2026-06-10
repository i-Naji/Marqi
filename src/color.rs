//! Terminal-color adaptation.
//!
//! Many terminals do not support 24-bit "truecolor"; on those, a `Color::Rgb`
//! is silently dropped to the default foreground (which reads as gray), while
//! 256-color indexed values still render. We detect truecolor once and, when it
//! is unavailable, downgrade RGB to the nearest xterm-256 index so the theme and
//! syntax highlighting look right everywhere.

use std::sync::OnceLock;

use ratatui::style::Color;

/// Whether the terminal advertises truecolor. Detected once, from `COLORTERM`
/// (the de-facto standard) or `WT_SESSION` (Windows Terminal, which supports
/// truecolor but does not set `COLORTERM`).
pub fn truecolor() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        matches!(
            std::env::var("COLORTERM").as_deref(),
            Ok("truecolor") | Ok("24bit")
        ) || std::env::var_os("WT_SESSION").is_some()
    })
}

/// An RGB color, downgraded to xterm-256 when truecolor is unavailable.
pub fn rgb(r: u8, g: u8, b: u8) -> Color {
    if truecolor() {
        Color::Rgb(r, g, b)
    } else {
        Color::Indexed(rgb_to_ansi256(r, g, b))
    }
}

/// Parse a `#rrggbb` (or `rrggbb`) hex string into an adapted [`Color`].
pub fn parse_hex(s: &str) -> Option<Color> {
    let s = s.strip_prefix('#').unwrap_or(s);
    // The ASCII check also keeps the fixed-offset slices below on char
    // boundaries; without it a multibyte char in a config value would panic.
    if s.len() != 6 || !s.is_ascii() {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(rgb(r, g, b))
}

/// Map an RGB triple to the nearest xterm-256 palette colour: whichever of the
/// 6×6×6 colour cube (16–231) or the 24-step grey ramp (232–255) is closest in
/// RGB space. Picking the closer of the two keeps vivid colours vivid while
/// mapping dark, desaturated colours (e.g. a code-block background) to a neutral
/// grey instead of a wrong, saturated cube vertex that would tint them.
pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];

    // Nearest colour-cube vertex (per channel) and the colour it represents.
    let cube_idx = |v: u8| {
        (0..6)
            .min_by_key(|&i| (STEPS[i] as i16 - v as i16).abs())
            .unwrap()
    };
    let (ri, gi, bi) = (cube_idx(r), cube_idx(g), cube_idx(b));
    let cube = (16 + 36 * ri + 6 * gi + bi) as u8;
    let (cr, cg, cb) = (STEPS[ri], STEPS[gi], STEPS[bi]);

    // Nearest grey from the 24-step ramp (values 8, 18, … 238).
    let avg = (r as u16 + g as u16 + b as u16) / 3;
    let gray_idx = ((avg as i16 - 3) / 10).clamp(0, 23) as u8;
    let gv = 8 + 10 * gray_idx;
    let gray = 232 + gray_idx;

    let dist = |x: u8, y: u8, z: u8| {
        let sq = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        sq(x, r) + sq(y, g) + sq(z, b)
    };
    if dist(cr, cg, cb) <= dist(gv, gv, gv) {
        cube
    } else {
        gray
    }
}

#[cfg(test)]
mod tests {
    use super::rgb_to_ansi256;

    #[test]
    fn maps_pure_colors_into_the_cube() {
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16); // black
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231); // white (top of cube)
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196); // red
        assert_eq!(rgb_to_ansi256(0, 255, 0), 46); // green
        assert_eq!(rgb_to_ansi256(0, 0, 255), 21); // blue
    }

    #[test]
    fn maps_mid_gray_into_the_gray_ramp() {
        let idx = rgb_to_ansi256(128, 128, 128);
        assert!((232..=255).contains(&idx), "expected gray ramp, got {idx}");
    }

    #[test]
    fn dark_desaturated_colors_use_the_gray_ramp_not_a_cube_vertex() {
        // A dark blue-gray code-block background must land on the gray ramp, not
        // snap to a saturated teal cube vertex.
        let idx = rgb_to_ansi256(0x2b, 0x30, 0x3b);
        assert!((232..=255).contains(&idx), "expected gray ramp, got {idx}");
    }
}
