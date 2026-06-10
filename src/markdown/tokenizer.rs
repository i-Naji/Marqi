//! Lossless source highlighting for the active (raw) block.
//!
//! Unlike the preview renderer, this never consults comrak's inline source
//! positions (which are unreliable on multibyte / multi-line inlines). Instead
//! it scans the block's raw source directly and assigns a [`Style`] to **every
//! byte** — markers (`#`, `**`, backticks, link brackets…) get the dim marker
//! style, content gets the matching emphasis/heading/code style. Because it only
//! assigns styles and never rewrites bytes, the rendered raw block always
//! reproduces the source exactly, which keeps the cursor's byte↔column mapping
//! valid.

use ratatui::style::{Modifier, Style};

use super::theme::MarkdownTheme;

/// One style per byte of `src` (`result.len() == src.len()`).
pub fn highlight(src: &str, theme: &MarkdownTheme) -> Vec<Style> {
    let mut styles = vec![theme.text; src.len()];
    let mut offset = 0;
    // `Some((char, run))` while between fence delimiters: interior lines are
    // code, not markdown — scanning them for emphasis/links/headings would
    // style `a * b` in a snippet as emphasis.
    let mut fence: Option<(u8, usize)> = None;
    for line in src.split_inclusive('\n') {
        let out = &mut styles[offset..offset + line.len()];
        match fence {
            Some((open, open_run)) => match fence_delimiter(line) {
                // A closing fence: same char, at least the opening run, bare.
                Some((ch, run, true)) if ch == open && run >= open_run => {
                    highlight_line(line, out, theme);
                    fence = None;
                }
                _ => {
                    let content_len = line.trim_end_matches(['\n', '\r']).len();
                    fill(out, 0, content_len, theme.code_inline);
                }
            },
            None => {
                if let Some((ch, run, _)) = fence_delimiter(line) {
                    fence = Some((ch, run));
                }
                highlight_line(line, out, theme);
            }
        }
        offset += line.len();
    }
    styles
}

/// `Some((delimiter_char, run_len, rest_is_blank))` when the line starts with a
/// code-fence delimiter (3+ backticks or tildes after optional indentation).
fn fence_delimiter(line: &str) -> Option<(u8, usize, bool)> {
    let body = line
        .trim_start_matches([' ', '\t'])
        .trim_end_matches(['\n', '\r']);
    let first = *body.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let run = body.bytes().take_while(|b| *b == first).count();
    (run >= 3).then(|| (first, run, body[run..].trim().is_empty()))
}

fn highlight_line(line: &str, out: &mut [Style], theme: &MarkdownTheme) {
    let content_len = line.trim_end_matches(['\n', '\r']).len();
    let indent = content_len.min(line.len() - line.trim_start_matches([' ', '\t']).len());
    let body = &line[indent..content_len];

    // ATX heading: 1–6 '#'s followed by a space.
    let hashes = body.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hashes) && body[hashes..].starts_with(' ') {
        let marker_end = indent + hashes + 1;
        fill(out, indent, marker_end, theme.marker);
        fill(out, marker_end, content_len, theme.heading(hashes as u8));
        highlight_keywords(line, out, marker_end, content_len, theme);
        return;
    }

    // Block quote.
    if let Some(after) = body.strip_prefix('>') {
        let marker_end = indent + 1 + usize::from(after.starts_with(' '));
        fill(out, indent, marker_end, theme.quote_bar);
        inline_scan(line, out, marker_end, content_len, theme);
        highlight_keywords(line, out, marker_end, content_len, theme);
        return;
    }

    // Fenced code delimiter line.
    if body.starts_with("```") || body.starts_with("~~~") {
        fill(out, indent, content_len, theme.marker);
        return;
    }

    // List marker.
    if let Some(len) = list_marker_len(body) {
        fill(out, indent, indent + len, theme.list_marker);
        inline_scan(line, out, indent + len, content_len, theme);
        highlight_keywords(line, out, indent + len, content_len, theme);
        return;
    }

    inline_scan(line, out, indent, content_len, theme);
    highlight_keywords(line, out, indent, content_len, theme);
}

/// Length of a leading list marker (`- `, `* `, `+ `, `1. `, `2) `…), if any.
fn list_marker_len(body: &str) -> Option<usize> {
    let b = body.as_bytes();
    if b.len() >= 2 && matches!(b[0], b'-' | b'*' | b'+') && b[1] == b' ' {
        return Some(2);
    }
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0
        && b.len() > digits + 1
        && matches!(b[digits], b'.' | b')')
        && b[digits + 1] == b' '
    {
        return Some(digits + 2);
    }
    None
}

/// Scan inline markdown in `line[start..end]`, styling delimiters and content.
/// Emphasis is handled by a forgiving toggle scanner (not full CommonMark), which
/// is visually sensible while editing and always lossless.
fn inline_scan(line: &str, out: &mut [Style], start: usize, end: usize, theme: &MarkdownTheme) {
    let bytes = line.as_bytes();
    let base = theme.text;
    let mut i = start;
    let (mut bold, mut italic, mut strike) = (false, false, false);

    while i < end {
        match bytes[i] {
            b'`' => {
                let run = run_len(bytes, i, end, b'`');
                if let Some(close) = find_run(bytes, i + run, end, b'`', run) {
                    fill(out, i, i + run, theme.marker);
                    fill(out, i + run, close, theme.code_inline);
                    fill(out, close, close + run, theme.marker);
                    i = close + run;
                } else {
                    out[i] = base;
                    i += 1;
                }
            }
            b'*' => {
                let run = run_len(bytes, i, end, b'*');
                if run >= 2 {
                    bold = !bold;
                    fill(out, i, i + 2, theme.marker);
                    i += 2;
                } else {
                    italic = !italic;
                    out[i] = theme.marker;
                    i += 1;
                }
            }
            b'_' => {
                let run = run_len(bytes, i, end, b'_');
                if run >= 2 {
                    bold = !bold;
                    fill(out, i, i + 2, theme.marker);
                    i += 2;
                } else if underscore_opens_emphasis(line, i) {
                    italic = !italic;
                    out[i] = theme.marker;
                    i += 1;
                } else {
                    // GFM forbids intraword `_` emphasis, so an underscore
                    // flanked by word chars stays literal (matching the preview).
                    out[i] = emphasized(base, bold, italic, strike);
                    i += 1;
                }
            }
            b'~' if run_len(bytes, i, end, b'~') >= 2 => {
                strike = !strike;
                fill(out, i, i + 2, theme.marker);
                i += 2;
            }
            b'[' => {
                if let Some(rb) = find(bytes, i + 1, end, b']') {
                    fill(out, i, i + 1, theme.marker);
                    fill(out, i + 1, rb, theme.link);
                    out[rb] = theme.marker;
                    i = rb + 1;
                    if i < end
                        && bytes[i] == b'('
                        && let Some(rp) = matching_paren(bytes, i, end)
                    {
                        fill(out, i, rp + 1, theme.marker);
                        i = rp + 1;
                    }
                } else {
                    out[i] = base;
                    i += 1;
                }
            }
            _ => {
                out[i] = emphasized(base, bold, italic, strike);
                i += 1;
            }
        }
    }
}

fn emphasized(base: Style, bold: bool, italic: bool, strike: bool) -> Style {
    let mut s = base;
    if bold {
        s = s.add_modifier(Modifier::BOLD);
    }
    if italic {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if strike {
        s = s.add_modifier(Modifier::CROSSED_OUT);
    }
    s
}

fn fill(out: &mut [Style], start: usize, end: usize, style: Style) {
    let end = end.min(out.len());
    for s in &mut out[start.min(end)..end] {
        *s = style;
    }
}

fn highlight_keywords(
    line: &str,
    out: &mut [Style],
    start: usize,
    end: usize,
    theme: &MarkdownTheme,
) {
    let mut i = start;
    while i < end {
        let Some((kw_end, style)) = keyword_at(line, i, end, theme) else {
            i += 1;
            continue;
        };
        fill(out, i, kw_end, style);
        i = kw_end;
    }
}

fn keyword_at(line: &str, at: usize, end: usize, theme: &MarkdownTheme) -> Option<(usize, Style)> {
    if !line.is_char_boundary(at) {
        return None;
    }
    if !is_boundary_before(line, at) {
        return None;
    }
    for (word, style) in [
        ("IMPORTANT", theme.keyword_misc),
        ("WARNING", theme.keyword_warn),
        ("FIXME", theme.keyword_error),
        ("ERROR", theme.keyword_error),
        ("WARN", theme.keyword_warn),
        ("TODO", theme.keyword_note),
        ("NOTE", theme.keyword_note),
        ("HACK", theme.keyword_misc),
    ] {
        let kw_end = at + word.len();
        if kw_end <= end && line[at..].starts_with(word) && is_boundary_after(line, kw_end) {
            return Some((kw_end, style));
        }
    }
    None
}

/// Whether a single `_` at byte `at` can act as emphasis. GFM forbids intraword
/// underscore emphasis, so an underscore flanked by word characters on both
/// sides stays literal; otherwise it may open or close emphasis.
fn underscore_opens_emphasis(line: &str, at: usize) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let before = line[..at].chars().next_back();
    let after = line[at + 1..].chars().next();
    !(before.is_some_and(word) && after.is_some_and(word))
}

fn is_boundary_before(line: &str, at: usize) -> bool {
    at == 0
        || line[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn is_boundary_after(line: &str, at: usize) -> bool {
    at >= line.len()
        || line[at..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn run_len(bytes: &[u8], i: usize, end: usize, c: u8) -> usize {
    bytes[i..end].iter().take_while(|b| **b == c).count()
}

fn find(bytes: &[u8], from: usize, end: usize, target: u8) -> Option<usize> {
    (from..end).find(|&i| bytes[i] == target)
}

/// Index of the `)` matching the `(` at `open`, counting nesting so that a link
/// URL with balanced parentheses (e.g. `(https://e/a_(b))`) is not truncated at
/// the first inner `)`.
fn matching_paren(bytes: &[u8], open: usize, end: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (j, &b) in bytes.iter().enumerate().take(end).skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
    }
    None
}

/// Find a run of exactly `len` `c`s starting at or after `from`.
fn find_run(bytes: &[u8], from: usize, end: usize, c: u8, len: usize) -> Option<usize> {
    let mut j = from;
    while j < end {
        if bytes[j] == c {
            let r = run_len(bytes, j, end, c);
            if r == len {
                return Some(j);
            }
            j += r;
        } else {
            j += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_a_style_to_every_byte() {
        let theme = MarkdownTheme::default();
        let src = "# Heading\n\nA **bold** word and `code` and a [link](http://x).\n";
        let styles = highlight(src, &theme);
        assert_eq!(
            styles.len(),
            src.len(),
            "must be lossless: one style per byte"
        );
    }

    #[test]
    fn heading_hash_is_a_marker_and_text_is_heading_styled() {
        let theme = MarkdownTheme::default();
        let src = "## Title";
        let styles = highlight(src, &theme);
        assert_eq!(styles[0], theme.marker); // '#'
        assert_eq!(styles[1], theme.marker); // '#'
        assert_eq!(styles[3], theme.heading(2)); // 'T'
    }

    #[test]
    fn emphasis_markers_use_marker_style() {
        let theme = MarkdownTheme::default();
        let src = "a **b** c";
        let styles = highlight(src, &theme);
        let stars: Vec<usize> = src.match_indices('*').map(|(i, _)| i).collect();
        for i in stars {
            assert_eq!(styles[i], theme.marker, "star at {i} should be a marker");
        }
    }

    #[test]
    fn keywords_are_highlighted_losslessly() {
        let theme = MarkdownTheme::default();
        let src = "TODO and ERROR";
        let styles = highlight(src, &theme);
        assert_eq!(styles.len(), src.len());
        assert_eq!(styles[0], theme.keyword_note);
        assert_eq!(styles[9], theme.keyword_error);
    }

    #[test]
    fn intraword_underscore_stays_literal() {
        let theme = MarkdownTheme::default();
        let src = "foo_bar_baz";
        let styles = highlight(src, &theme);
        for (i, b) in src.bytes().enumerate() {
            if b == b'_' {
                assert_ne!(
                    styles[i], theme.marker,
                    "underscore at {i} should be literal"
                );
            }
        }
    }

    #[test]
    fn flanked_underscore_is_emphasis() {
        let theme = MarkdownTheme::default();
        let src = "a _x_ b";
        let styles = highlight(src, &theme);
        for (i, b) in src.bytes().enumerate() {
            if b == b'_' {
                assert_eq!(
                    styles[i], theme.marker,
                    "underscore at {i} should be a marker"
                );
            }
        }
    }

    #[test]
    fn fenced_code_interiors_are_not_scanned_as_markdown() {
        let theme = MarkdownTheme::default();
        let src = "```py\na = b * c\n# comment\n```\nafter *em*\n";
        let styles = highlight(src, &theme);
        assert_eq!(styles.len(), src.len());
        // `*` inside the fence is code, not an emphasis marker.
        let star = src.find('*').unwrap();
        assert_eq!(styles[star], theme.code_inline);
        // `#` inside the fence is code, not a heading.
        let hash = src.find('#').unwrap();
        assert_eq!(styles[hash], theme.code_inline);
        // After the closing fence, markdown scanning resumes.
        let em_star = src.rfind('*').unwrap();
        assert_eq!(styles[em_star], theme.marker);
    }

    #[test]
    fn unclosed_fence_runs_to_the_end() {
        let theme = MarkdownTheme::default();
        let src = "```\n**not bold**";
        let styles = highlight(src, &theme);
        let star = src.find('*').unwrap();
        assert_eq!(styles[star], theme.code_inline);
    }

    #[test]
    fn link_url_with_balanced_parens_is_dimmed_whole() {
        let theme = MarkdownTheme::default();
        let src = "[x](http://e.com/a_(b))";
        let styles = highlight(src, &theme);
        // The entire "(...)" target, including the inner ')', is dimmed as a
        // marker rather than being truncated at the first ')'.
        let open = src.find('(').unwrap();
        for (i, style) in styles.iter().enumerate().skip(open) {
            assert_eq!(*style, theme.marker, "url byte {i} should be a marker");
        }
    }
}
