//! Deterministic document and edit generators shared by tests.
//!
//! Randomness is a hand-rolled xorshift (no dependencies) so any failing case
//! replays exactly from its seed. Generators cover the shapes the engine
//! rework cares about: large ASCII, CJK/emoji/combining marks, tabs and
//! soft-wrap, many markdown blocks, and a single giant block.

// The generators land ahead of their consumers; later phases' tests pick
// them up one by one.
#![allow(dead_code)]

use ropey::Rope;

/// xorshift64* — deterministic, seedable, dependency-free.
pub struct XorShift(u64);

impl XorShift {
    pub fn new(seed: u64) -> Self {
        // xorshift state must be non-zero.
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform value in `0..n` (`n` must be non-zero).
    pub fn below(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        (self.next_u64() % n as u64) as usize
    }

    /// True with probability `num/den`.
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.next_u64() % den < num
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// `lines` ASCII lines of exactly `line_len` letters each.
pub fn ascii(lines: usize, line_len: usize) -> String {
    let mut out = String::with_capacity(lines * (line_len + 1));
    for i in 0..lines {
        for j in 0..line_len {
            out.push(char::from(b'a' + ((i + j) % 26) as u8));
        }
        out.push('\n');
    }
    out
}

/// Lines mixing CJK (width 2), emoji ZWJ families, skin tones, and combining
/// marks — every grapheme-width edge the layout has to handle.
pub fn cjk_emoji(lines: usize) -> String {
    const TEMPLATES: [&str; 5] = [
        "中文加粗测试一行 with ascii tail",
        "日本語の斜体テキスト",
        "family 👨\u{200d}👩\u{200d}👧\u{200d}👦 and thumbs 👍🏽 emoji",
        "combining: cafe\u{301} re\u{301}sume\u{301}",
        "mixed 中a文b字c with 🎉 narrow gaps",
    ];
    let mut out = String::new();
    for i in 0..lines {
        out.push_str(TEMPLATES[i % TEMPLATES.len()]);
        out.push('\n');
    }
    out
}

/// Lines with leading/interior tabs and lines long enough to soft-wrap at
/// typical viewport widths.
pub fn tabs_and_wrap(lines: usize) -> String {
    let mut out = String::new();
    for i in 0..lines {
        match i % 4 {
            0 => out.push_str("\tindented with a leading tab\n"),
            1 => out.push_str("col\ta\tb\tc\ttab separated cells\n"),
            2 => {
                out.push_str(&"wrap ".repeat(40));
                out.push('\n');
            }
            _ => out.push_str("short line\n"),
        }
    }
    out
}

/// `n` markdown blocks cycling through every construct the renderer handles,
/// separated by blank lines. Includes a reference definition + use and a
/// footnote pair so cross-block state is always present.
pub fn many_blocks(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        match i % 12 {
            0 => out.push_str(&format!("# Heading {i}\n")),
            1 => out.push_str(&format!(
                "Paragraph {i} with **bold**, *italic*, `code`, and a [ref link][r{}].\n",
                i % 3
            )),
            2 => out.push_str("- bullet one\n- bullet two\n  - nested\n"),
            3 => out.push_str(&format!("1. first {i}\n2. second\n3. third\n")),
            4 => out.push_str("- [ ] open task\n- [x] done task\n"),
            5 => out.push_str("```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n"),
            6 => out.push_str("> quoted text\n> > nested quote\n"),
            7 => out.push_str("| a | b |\n| --- | ---: |\n| 1 | 2 |\n"),
            8 => out.push_str(&format!("[r{}]: https://example.com/{i}\n", i % 3)),
            9 => out.push_str(&format!("Setext heading {i}\n===\n")),
            10 => out.push_str(&format!(
                "Footnote use[^f{i}].\n\n[^f{i}]: its definition\n"
            )),
            _ => out.push_str("---\n"),
        }
        out.push('\n');
    }
    out
}

/// One giant paragraph (no blank lines, so comrak sees a single block) of at
/// least `bytes` bytes.
pub fn giant_block(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 64);
    let mut word = 0usize;
    while out.len() < bytes {
        out.push_str("lorem ipsum dolor sit amet ");
        word += 1;
        if word.is_multiple_of(12) {
            out.push('\n'); // hard lines, but never blank — still one block
        }
    }
    out.push('\n');
    out
}

/// A document of roughly `lines` lines assembled from random construct
/// fragments — paragraphs, lists, fences, tables, quotes, defs, blanks.
pub fn random_doc(rng: &mut XorShift, lines: usize) -> String {
    const FRAGMENTS: [&str; 10] = [
        "plain paragraph text with some words\n",
        "# a heading\n",
        "- list item\n",
        "1. numbered item\n",
        "> quote line\n",
        "```\ncode body\n```\n",
        "| a | b |\n| - | - |\n",
        "[d]: https://example.org\n",
        "\n",
        "text with `inline` and **strong**\n",
    ];
    let mut out = String::new();
    while out.lines().count() < lines {
        out.push_str(rng.pick(&FRAGMENTS));
        if rng.chance(1, 3) {
            out.push('\n');
        }
    }
    out
}

/// A random splice spec `(start_byte, end_byte, replacement)` that is always
/// char-boundary safe for `rope`. Inserts (empty range), deletes (empty
/// replacement), and replacements are all generated.
pub fn random_edit(rng: &mut XorShift, rope: &Rope) -> (usize, usize, String) {
    const PIECES: [&str; 14] = [
        "x", "words ", "`", "~", "#", "-", "|", ">", "=", "[", "]:", "\n", "中", "👍",
    ];
    let len_chars = rope.len_chars();
    let a = rng.below(len_chars + 1);
    let start = rope.char_to_byte(a);
    let (end, text) = if rng.chance(1, 2) {
        // Insert.
        (start, build_insert(rng, &PIECES))
    } else {
        let b = (a + rng.below(64).max(1)).min(len_chars);
        let end = rope.char_to_byte(b);
        if rng.chance(1, 2) {
            (end, String::new()) // delete
        } else {
            (end, build_insert(rng, &PIECES)) // replace
        }
    };
    (start, end, text)
}

fn build_insert(rng: &mut XorShift, pieces: &[&str]) -> String {
    let mut text = String::new();
    for _ in 0..rng.below(5) + 1 {
        text.push_str(rng.pick(pieces));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_are_deterministic_and_well_formed() {
        let mut a = XorShift::new(7);
        let mut b = XorShift::new(7);
        assert_eq!(
            (a.next_u64(), a.next_u64()),
            (b.next_u64(), b.next_u64()),
            "same seed, same sequence"
        );

        assert_eq!(ascii(10, 20).lines().count(), 10);
        assert!(ascii(2, 8).lines().all(|l| l.len() == 8));
        assert_eq!(cjk_emoji(7).lines().count(), 7);
        assert!(tabs_and_wrap(8).contains('\t'));
        let blocks = many_blocks(24);
        assert!(
            blocks.contains("```rust")
                && blocks.contains("]: https://example.com")
                && blocks.contains("[^f")
        );
        assert!(giant_block(10_000).len() >= 10_000);
        assert!(!giant_block(1_000).contains("\n\n"), "stays a single block");

        let mut rng = XorShift::new(42);
        assert!(random_doc(&mut rng, 50).lines().count() >= 50);
        let rope = Rope::from_str(&cjk_emoji(20));
        for _ in 0..500 {
            let (start, end, text) = random_edit(&mut rng, &rope);
            assert!(start <= end && end <= rope.len_bytes());
            // Splicing at these bounds must be valid — exercise it.
            let mut clone = rope.clone();
            clone.remove(clone.byte_to_char(start)..clone.byte_to_char(end));
            clone.insert(clone.byte_to_char(start), &text);
        }
    }
}
