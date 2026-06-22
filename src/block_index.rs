//! The document's Markdown block index.
//!
//! comrak's whole-document parse is the single source of truth for top-level
//! block boundaries (so the hybrid view and read preview agree, and constructs
//! like setext headings, indented code, and loose lists group exactly as
//! rendered). This module owns that truth: [`BlockIndex::rebuild_full`] is the
//! ported whole-document partition (also the test oracle); incremental
//! windowed reparsing is layered on top, with a full reparse as the fallback
//! for anything the windowed scheme cannot prove safe.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use comrak::nodes::NodeValue;
use comrak::{Arena, parse_document};
use ropey::Rope;

use crate::markdown::gfm_options;

/// One top-level block: a maximal run of source lines owned by a single
/// top-level comrak node.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceBlock {
    pub start_line: usize,
    /// Inclusive.
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    /// Hash of the block's source slice, computed once at parse time so
    /// per-build cache keys never re-slice or re-hash block text.
    pub content_hash: u64,
    /// The owning node's kind — drives the windowed-reparse seam rules.
    pub kind: BlockKind,
}

/// Top-level block kinds, as far as the seam rules care.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockKind {
    Paragraph,
    Heading,
    List,
    IndentedCode,
    FencedCode,
    Html,
    Table,
    Quote,
    FootnoteDef,
    Rule,
    Other,
}

#[derive(Default, Clone, Copy)]
pub struct BlockIndexStats {
    /// Whole-document reparses (first build, fallbacks, backstop).
    pub full_rebuilds: usize,
    /// Duration of the last whole-document partition.
    pub last_parse_us: u128,
}

/// Block boundaries + link reference definitions, versioned against the
/// buffer. `is_current` mismatches degrade to a full rebuild — never to a
/// stale index.
pub struct BlockIndex {
    blocks: Vec<SourceBlock>,
    /// Link reference definitions by source line: `[docs]: url` lines living
    /// in the gaps between blocks (comrak consumes them into its refmap, so
    /// they never appear in the AST). The flat `ref_defs` string is their
    /// concatenation in line order.
    defs: BTreeMap<usize, String>,
    ref_defs: String,
    ref_defs_hash: u64,
    version: Option<u64>,
    stats: BlockIndexStats,
}

impl Default for BlockIndex {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            defs: BTreeMap::new(),
            ref_defs: String::new(),
            ref_defs_hash: hash_str(""),
            version: None,
            stats: BlockIndexStats::default(),
        }
    }
}

impl BlockIndex {
    pub fn blocks(&self) -> &[SourceBlock] {
        &self.blocks
    }

    /// The document's link reference definitions, one per line, in line order.
    pub fn ref_defs(&self) -> &str {
        &self.ref_defs
    }

    pub fn ref_defs_hash(&self) -> u64 {
        self.ref_defs_hash
    }

    pub fn is_current(&self, version: u64) -> bool {
        self.version == Some(version)
    }

    pub fn stats(&self) -> BlockIndexStats {
        self.stats
    }

    /// Whole-document reparse: the ground truth, used for the first build and
    /// as the fallback for every case windowed reparsing cannot prove safe.
    pub fn rebuild_full(&mut self, rope: &Rope, version: u64) {
        let started = std::time::Instant::now();
        let (blocks, defs) = parse_blocks(rope);
        self.blocks = blocks;
        self.defs = defs;
        self.rebuild_ref_defs();
        self.version = Some(version);
        self.stats.full_rebuilds += 1;
        self.stats.last_parse_us = started.elapsed().as_micros();
    }

    /// Re-derive the flat defs string (+ hash) from the line-keyed registry.
    fn rebuild_ref_defs(&mut self) {
        self.ref_defs.clear();
        for def in self.defs.values() {
            self.ref_defs.push_str(def);
            self.ref_defs.push('\n');
        }
        self.ref_defs_hash = hash_str(&self.ref_defs);
    }
}

fn hash_str(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Partition `rope` into top-level blocks and collect the link reference
/// definitions, exactly as the whole-document comrak parse sees them. This is
/// the single source of truth — and the oracle the incremental path is
/// equivalence-tested against.
pub fn parse_blocks(rope: &Rope) -> (Vec<SourceBlock>, BTreeMap<usize, String>) {
    let total = rope.len_lines();
    if total == 0 {
        return (Vec::new(), BTreeMap::new());
    }
    let source = rope.to_string();
    let arena = Arena::new();
    let root = parse_document(&arena, &source, &gfm_options());

    // Map each line to its owning top-level child, then group maximal
    // same-owner runs into blocks. Unowned lines are blank gaps (or pure
    // definitions) the caller renders raw.
    let mut owner: Vec<Option<usize>> = vec![None; total];
    let mut kinds: Vec<BlockKind> = Vec::new();
    for (idx, node) in root.children().enumerate() {
        kinds.push(block_kind(&node.data.borrow().value));
        let sp = node.data.borrow().sourcepos;
        let start = sp.start.line.saturating_sub(1).min(total - 1);
        let end = sp.end.line.saturating_sub(1).min(total - 1);
        for slot in owner.iter_mut().take(end + 1).skip(start) {
            *slot = Some(idx);
        }
    }

    let mut blocks = Vec::new();
    let mut line = 0;
    while line < total {
        let Some(idx) = owner[line] else {
            line += 1;
            continue;
        };
        let start = line;
        while line < total && owner[line] == Some(idx) {
            line += 1;
        }
        let end = line - 1;
        let start_byte = rope.line_to_byte(start);
        let end_byte = if end + 1 < total {
            rope.line_to_byte(end + 1)
        } else {
            rope.len_bytes()
        };
        blocks.push(SourceBlock {
            start_line: start,
            end_line: end,
            start_byte,
            end_byte,
            content_hash: hash_str(&source[start_byte..end_byte]),
            kind: kinds[idx],
        });
    }
    (blocks, defs_from_gaps(rope, &owner))
}

fn block_kind(value: &NodeValue) -> BlockKind {
    match value {
        NodeValue::Paragraph => BlockKind::Paragraph,
        NodeValue::Heading(_) => BlockKind::Heading,
        NodeValue::List(_) => BlockKind::List,
        NodeValue::CodeBlock(code) => {
            if code.fenced {
                BlockKind::FencedCode
            } else {
                BlockKind::IndentedCode
            }
        }
        NodeValue::HtmlBlock(_) => BlockKind::Html,
        NodeValue::Table(_) => BlockKind::Table,
        NodeValue::BlockQuote => BlockKind::Quote,
        NodeValue::FootnoteDefinition(_) => BlockKind::FootnoteDef,
        NodeValue::ThematicBreak => BlockKind::Rule,
        _ => BlockKind::Other,
    }
}

/// Collect the document's link reference definitions, keyed by source line.
///
/// Definitions never appear in the AST — comrak consumes them into its
/// refmap — so their lines are exactly the non-blank *unowned* ones. A line is
/// taken only if it alone re-parses to an empty document, the signature of a
/// pure definition (a look-alike inside a paragraph or code block is owned and
/// never reaches the check). Footnote definitions are skipped: appending one
/// would make it render inside any block that references it.
fn defs_from_gaps(rope: &Rope, owner: &[Option<usize>]) -> BTreeMap<usize, String> {
    // comrak itself decides what qualifies. Footnotes are disabled for the
    // probe so a `[^name]:` definition parses as a paragraph and is rejected.
    let mut probe_options = gfm_options();
    probe_options.extension.footnotes = false;
    let mut out = BTreeMap::new();
    for (idx, owned) in owner.iter().enumerate() {
        if owned.is_some() {
            continue;
        }
        let line = rope.line(idx).to_string();
        if line.trim().is_empty() {
            continue;
        }
        let arena = Arena::new();
        if parse_document(&arena, &line, &probe_options)
            .children()
            .next()
            .is_none()
        {
            out.insert(idx, line.trim().to_string());
        }
    }
    out
}

/// Byte-range slice of the rope as an owned string.
pub fn source_slice(rope: &Rope, start: usize, end: usize) -> String {
    rope.slice(rope.byte_to_char(start)..rope.byte_to_char(end))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ported partition must agree with itself across versions and shapes
    /// (the real equivalence work starts when windowed reparsing lands — this
    /// pins the registry semantics and the harness).
    #[test]
    fn registry_collects_exactly_the_gap_definitions() {
        let src = "\
para one with [a][r1] and [b][r2].

[r1]: https://one.example

a paragraph
[looks]: like-a-def but this lazy continuation keeps it owned
so it is paragraph text, not a definition

[r2]: https://two.example

[^note]: footnote definitions are skipped
";
        let rope = Rope::from_str(src);
        let (blocks, defs) = parse_blocks(&rope);
        assert!(!blocks.is_empty());
        let collected: Vec<&str> = defs.values().map(String::as_str).collect();
        assert_eq!(
            collected,
            ["[r1]: https://one.example", "[r2]: https://two.example"],
            "exactly the pure gap definitions, in line order"
        );

        let mut index = BlockIndex::default();
        index.rebuild_full(&rope, 0);
        assert!(index.is_current(0));
        assert_eq!(
            index.ref_defs(),
            "[r1]: https://one.example\n[r2]: https://two.example\n"
        );
        assert_eq!(index.blocks().len(), blocks.len());
    }

    #[test]
    fn block_kinds_cover_the_seam_relevant_constructs() {
        let src = "\
# heading

paragraph

    indented code

- list

```
fenced
```

<div>html</div>

| a |
| - |

> quote

---

[^n]: def (referenced[^n] above? no — here)
";
        let rope = Rope::from_str(src);
        let (blocks, _) = parse_blocks(&rope);
        let kinds: Vec<BlockKind> = blocks.iter().map(|b| b.kind).collect();
        use BlockKind::*;
        assert_eq!(
            kinds,
            [
                Heading,
                Paragraph,
                IndentedCode,
                List,
                FencedCode,
                Html,
                Table,
                Quote,
                Rule,
                FootnoteDef
            ]
        );
    }
}
