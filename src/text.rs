//! Grapheme-cluster boundary lookups over a rope.
//!
//! Cursor motion and deletion must step by whole grapheme clusters, never by
//! `char` or byte — otherwise a `👨‍👩‍👧‍👦` ZWJ sequence or a combining-mark cluster
//! would be split and the buffer corrupted. These helpers feed rope chunks to
//! [`unicode_segmentation::GraphemeCursor`], which finds boundaries without
//! materialising the whole document as a string.

use ropey::Rope;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

/// Byte offset of the grapheme boundary at or after `byte` (i.e. the start of
/// the next cluster). Returns `rope.len_bytes()` at the end of the document.
pub fn next_grapheme(rope: &Rope, byte: usize) -> usize {
    let len = rope.len_bytes();
    if byte >= len {
        return len;
    }
    let mut cursor = GraphemeCursor::new(byte, len, true);
    let (mut chunk, mut chunk_start, _, _) = rope.chunk_at_byte(byte);
    loop {
        match cursor.next_boundary(chunk, chunk_start) {
            Ok(Some(n)) => return n,
            Ok(None) => return len,
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_start += chunk.len();
                chunk = rope.chunk_at_byte(chunk_start).0;
            }
            Err(GraphemeIncomplete::PreContext(idx)) => {
                // `idx` is always a chunk boundary here, so the chunk holding
                // `idx - 1` ends exactly at `idx` — which is what
                // `provide_context` asserts (`ctx_start + ctx.len() == idx`).
                let (ctx, ctx_start, _, _) = rope.chunk_at_byte(idx - 1);
                cursor.provide_context(ctx, ctx_start);
            }
            _ => unreachable!("unexpected grapheme cursor state moving forward"),
        }
    }
}

/// Byte offset of the grapheme boundary strictly before `byte` (i.e. the start
/// of the previous cluster). Returns `0` at the start of the document.
pub fn prev_grapheme(rope: &Rope, byte: usize) -> usize {
    if byte == 0 {
        return 0;
    }
    let len = rope.len_bytes();
    let mut cursor = GraphemeCursor::new(byte, len, true);
    // chunk_at_byte(len) is valid, but probe one byte back so we land in the
    // chunk that actually contains content before the cursor.
    let probe = byte.min(len.saturating_sub(1));
    let (mut chunk, mut chunk_start, _, _) = rope.chunk_at_byte(probe);
    loop {
        match cursor.prev_boundary(chunk, chunk_start) {
            Ok(Some(n)) => return n,
            Ok(None) => return 0,
            Err(GraphemeIncomplete::PrevChunk) => {
                let (c, cs, _, _) = rope.chunk_at_byte(chunk_start - 1);
                chunk = c;
                chunk_start = cs;
            }
            Err(GraphemeIncomplete::PreContext(idx)) => {
                // As above: `idx` lands on a chunk boundary, satisfying
                // `provide_context`'s end-at-`idx` assertion.
                let (ctx, ctx_start, _, _) = rope.chunk_at_byte(idx - 1);
                cursor.provide_context(ctx, ctx_start);
            }
            _ => unreachable!("unexpected grapheme cursor state moving backward"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk the whole rope forward one grapheme at a time and collect the
    /// boundary offsets, then walk back and confirm they match in reverse.
    fn boundaries(text: &str) -> Vec<usize> {
        let rope = Rope::from_str(text);
        let mut offs = vec![0];
        let mut b = 0;
        while b < rope.len_bytes() {
            b = next_grapheme(&rope, b);
            offs.push(b);
        }
        offs
    }

    #[test]
    fn steps_over_cjk_and_emoji_clusters() {
        // "a中👍🏽👨‍👩‍👧‍👦b": ascii, CJK, skin-tone emoji, ZWJ family, ascii.
        let text = "a中👍🏽👨‍👩‍👧‍👦b";
        let rope = Rope::from_str(text);
        let offs = boundaries(text);

        // Each consecutive pair is exactly one grapheme cluster.
        let clusters: Vec<&str> = offs.windows(2).map(|w| &text[w[0]..w[1]]).collect();
        assert_eq!(clusters, vec!["a", "中", "👍🏽", "👨‍👩‍👧‍👦", "b"]);

        // prev_grapheme is the inverse of next_grapheme.
        for w in offs.windows(2) {
            assert_eq!(next_grapheme(&rope, w[0]), w[1]);
            assert_eq!(prev_grapheme(&rope, w[1]), w[0]);
        }
    }

    #[test]
    fn clamps_at_both_ends() {
        let rope = Rope::from_str("hi");
        assert_eq!(prev_grapheme(&rope, 0), 0);
        assert_eq!(next_grapheme(&rope, rope.len_bytes()), rope.len_bytes());
    }

    #[test]
    fn handles_combining_marks_as_one_cluster() {
        // "é" as e + U+0301 combining acute is a single grapheme.
        let text = "e\u{0301}x";
        let offs = boundaries(text);
        let clusters: Vec<&str> = offs.windows(2).map(|w| &text[w[0]..w[1]]).collect();
        assert_eq!(clusters, vec!["e\u{0301}", "x"]);
    }
}
