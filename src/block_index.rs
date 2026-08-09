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

use crate::line_index::{self, LineIndex, LineUpdate};
use crate::markdown::gfm_options;

/// Documents below this size always take the full-reparse path: it costs
/// well under a millisecond there, and the windowed machinery only earns its
/// complexity on large documents.
const FULL_FLOOR_BYTES: usize = 64 * 1024;
/// How far above the dirty block the upper-seam search looks for a sound
/// boundary before giving up. Blank gap lines occur every few lines in prose,
/// so this is generous; a blankless wall this tall falls back to a full parse.
const SEAM_SEARCH_LINES: usize = 256;

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

/// The shape of one buffer edit, in the same pre-mutation coordinates as
/// `Layout::update_after_edit` (plus the byte delta for suffix shifting).
#[derive(Clone, Copy)]
pub struct EditSpan {
    pub start_line: usize,
    pub old_line_count: usize,
    pub new_line_count: usize,
    /// `inserted_len - removed_len`.
    pub byte_delta: isize,
}

#[derive(Default, Clone, Copy)]
pub struct BlockIndexStats {
    /// Whole-document reparses (first build, fallbacks, backstop).
    pub full_rebuilds: usize,
    /// Edits absorbed by a windowed reparse.
    pub windowed_updates: usize,
    /// Two-sided windows whose post-parse invariants failed, forcing a wider
    /// attempt or the tail window.
    pub windows_grown: usize,
    /// Windowed attempts that gave up (no sound seam within the search cap).
    pub full_fallbacks: usize,
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
    full_floor_bytes: usize,
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
            full_floor_bytes: FULL_FLOOR_BYTES,
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

    #[cfg(test)]
    pub fn defs(&self) -> &BTreeMap<usize, String> {
        &self.defs
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

    #[cfg(test)]
    pub fn set_full_floor_bytes(&mut self, bytes: usize) {
        self.full_floor_bytes = bytes;
    }

    /// Incrementally absorb one edit, reparsing only a window around the
    /// dirty lines — `[U, V]` between two sound blank seams when possible,
    /// `[U, end-of-document]` otherwise — or falling back to a full reparse
    /// whenever safety cannot be proven. Trigger list:
    ///
    /// - the index is not exactly one version behind (missed edit) → mark
    ///   stale and let the `ensure_blocks` backstop rebuild;
    /// - document below the size floor → full (trivially-correct path);
    /// - the edit touched footnote or definition-shaped lines (pre or post) →
    ///   full (both have document-global effects);
    /// - footnote syntax anywhere in the window → widen to the tail or full
    ///   (a definition whose reference lives outside the window parses
    ///   differently in isolation);
    /// - no sound seam within the search caps → full.
    ///
    /// Upper seam soundness: `U == 0`, or `U` is a pre-edit *gap* blank line
    /// (no open fence/HTML/loose-list interior spans it — those lines are
    /// owned) whose preceding block cannot continue downward across a blank
    /// (lists, indented code, and footnote definitions can; everything else
    /// needs adjacency). Lines above the dirty block are byte-identical
    /// pre/post edit, so the parser state entering `U` is unchanged and fresh.
    ///
    /// Lower seam soundness: `V` is a blank line that was a pre-edit gap, the
    /// windowed parse leaves it unowned (an unclosed fence or HTML block
    /// would run into it), and the window's last block cannot absorb text
    /// across a blank — both checked *after* parsing, with window growth and
    /// then the tail window as fallbacks. The suffix blocks below `V` then
    /// reparse from the same closed, fresh state as before the edit, so the
    /// old entries are reused verbatim (shifted by the edit's line and byte
    /// deltas).
    pub fn apply_edit(
        &mut self,
        rope: &Rope,
        span: EditSpan,
        update: &LineUpdate,
        lines: &LineIndex,
        new_version: u64,
    ) {
        match self.version {
            Some(version) if version + 1 == new_version => {}
            _ => {
                // Out of sequence: degrade to the backstop, never patch.
                self.version = None;
                return;
            }
        }
        self.version = None; // pessimistic until one of the paths succeeds

        if rope.len_bytes() < self.full_floor_bytes {
            return self.rebuild_full(rope, new_version);
        }
        if update.touched_flags & (line_index::FOOTNOTE | line_index::DEF_COLON) != 0 {
            return self.rebuild_full(rope, new_version);
        }

        let total = rope.len_lines();
        let start_line = span.start_line.min(total.saturating_sub(1));
        // Blocks reparse whole or not at all: the window starts at or above
        // the start of the pre-edit block containing the first dirty line.
        let dirty_start = match self.block_at_line(start_line) {
            Some(i) => self.blocks[i].start_line,
            None => start_line,
        };
        let Some(seam) = self.upper_seam(dirty_start, lines) else {
            self.stats.full_fallbacks += 1;
            return self.rebuild_full(rope, new_version);
        };

        // Everything at/after this line is dirty for parsing: the edited
        // lines themselves, plus any lines whose fence state changed.
        let line_delta = span.new_line_count as isize - span.old_line_count as isize;
        let dirty_end = (start_line + span.new_line_count)
            .max(update.resynced_at)
            .saturating_sub(1);

        // Two-sided attempts, skipping further down after each failure.
        let mut from = dirty_end + 1;
        for attempt in 0..3 {
            let Some(v) = self.lower_seam(from, line_delta, lines, total) else {
                break;
            };
            if lines.any_flags_in(seam..v + 1, line_index::FOOTNOTE) {
                break; // footnotes in reach: decide between tail and full below
            }
            if self
                .try_window(rope, seam, v, line_delta, span.byte_delta, new_version)
                .is_ok()
            {
                self.stats.windowed_updates += 1;
                return;
            }
            self.stats.windows_grown += 1;
            from = v + 1 + SEAM_SEARCH_LINES * (attempt + 1);
        }

        // Tail window [seam, end): correct whenever the upper seam is.
        if lines.any_flags_in(seam..total, line_index::FOOTNOTE) {
            self.stats.full_fallbacks += 1;
            return self.rebuild_full(rope, new_version);
        }
        self.window_tail(rope, seam, new_version);
        self.stats.windowed_updates += 1;
    }

    /// Parse `[seam, v]` (inclusive of the blank seam line `v`) and splice it
    /// between the untouched prefix and the shifted suffix — or report that
    /// the lower seam is unsound.
    fn try_window(
        &mut self,
        rope: &Rope,
        seam: usize,
        v: usize,
        line_delta: isize,
        byte_delta: isize,
        new_version: u64,
    ) -> Result<(), ()> {
        let total = rope.len_lines();
        let seam_byte = rope.line_to_byte(seam);
        let end_byte = if v + 1 < total {
            rope.line_to_byte(v + 1)
        } else {
            rope.len_bytes()
        };
        let window = Rope::from_str(&source_slice(rope, seam_byte, end_byte));
        let (mut blocks, defs) = parse_blocks(&window);

        if let Some(last) = blocks.last() {
            // An unclosed fence/HTML block runs into the trailing blank...
            if seam + last.end_line >= v {
                return Err(());
            }
            // ...and these kinds can absorb suffix text across the blank.
            if open_below(last.kind) {
                return Err(());
            }
        }

        for block in &mut blocks {
            block.start_line += seam;
            block.end_line += seam;
            block.start_byte += seam_byte;
            block.end_byte += seam_byte;
        }
        let pre_v = v.saturating_add_signed(-line_delta);

        let mut rebuilt = Vec::with_capacity(self.blocks.len() + blocks.len());
        rebuilt.extend(self.blocks.iter().filter(|b| b.end_line < seam).cloned());
        rebuilt.extend(blocks);
        rebuilt.extend(
            self.blocks
                .iter()
                .filter(|b| b.start_line > pre_v)
                .map(|b| {
                    let mut block = b.clone();
                    block.start_line = block.start_line.saturating_add_signed(line_delta);
                    block.end_line = block.end_line.saturating_add_signed(line_delta);
                    block.start_byte = block.start_byte.saturating_add_signed(byte_delta);
                    block.end_byte = block.end_byte.saturating_add_signed(byte_delta);
                    block
                }),
        );
        self.blocks = rebuilt;

        let after_seam = self.defs.split_off(&seam);
        for (line, def) in defs {
            self.defs.insert(line + seam, def);
        }
        for (line, def) in after_seam {
            if line > pre_v {
                self.defs
                    .insert(line.saturating_add_signed(line_delta), def);
            }
        }
        self.rebuild_ref_defs();
        self.version = Some(new_version);
        Ok(())
    }

    /// Reparse `[seam, end-of-document)` and splice it after the prefix.
    fn window_tail(&mut self, rope: &Rope, seam: usize, new_version: u64) {
        let seam_byte = rope.line_to_byte(seam);
        // Re-deriving through a temporary rope reuses the exact
        // whole-document machinery (`parse_blocks`), so the windowed result
        // can only differ from a full parse if the seam itself is unsound.
        let window = Rope::from_str(&source_slice(rope, seam_byte, rope.len_bytes()));
        let (mut blocks, defs) = parse_blocks(&window);
        for block in &mut blocks {
            block.start_line += seam;
            block.end_line += seam;
            block.start_byte += seam_byte;
            block.end_byte += seam_byte;
        }

        self.blocks.retain(|b| b.end_line < seam);
        self.blocks.extend(blocks);
        let _ = self.defs.split_off(&seam); // drop defs at/after the seam
        for (line, def) in defs {
            self.defs.insert(line + seam, def);
        }
        self.rebuild_ref_defs();
        self.version = Some(new_version);
    }

    /// First sound lower seam at or after `from`: a blank line that was a
    /// pre-edit gap (post-parse invariants are checked by `try_window`).
    fn lower_seam(
        &self,
        from: usize,
        line_delta: isize,
        lines: &LineIndex,
        total: usize,
    ) -> Option<usize> {
        let to = from.saturating_add(SEAM_SEARCH_LINES).min(total);
        for v in from..to {
            if lines.flags_at(v) & line_index::BLANK == 0 {
                continue;
            }
            let pre_v = v as isize - line_delta;
            if pre_v < 0 {
                continue;
            }
            if self.block_at_line(pre_v as usize).is_some() {
                continue; // owned blank pre-edit (fence/HTML/loose-list interior)
            }
            return Some(v);
        }
        None
    }

    /// Sound one-sided seam at or above `dirty_start` (see [`Self::apply_edit`]).
    fn upper_seam(&self, dirty_start: usize, lines: &LineIndex) -> Option<usize> {
        if dirty_start == 0 {
            return Some(0);
        }
        let floor = dirty_start.saturating_sub(SEAM_SEARCH_LINES);
        for cand in (floor..dirty_start).rev() {
            if lines.flags_at(cand) & line_index::BLANK == 0 {
                continue;
            }
            if self.block_at_line(cand).is_some() {
                continue; // owned blank (fence/HTML/loose-list interior)
            }
            let above = self.last_block_before(cand);
            if above.is_some_and(|block| open_above_gap(block.kind)) {
                continue;
            }
            return Some(cand);
        }
        (floor == 0).then_some(0)
    }

    /// Index of the block containing `line`, if any.
    fn block_at_line(&self, line: usize) -> Option<usize> {
        let idx = self
            .blocks
            .partition_point(|b| b.start_line <= line)
            .checked_sub(1)?;
        (self.blocks[idx].end_line >= line).then_some(idx)
    }

    /// The last block entirely above `line`.
    fn last_block_before(&self, line: usize) -> Option<&SourceBlock> {
        let idx = self
            .blocks
            .partition_point(|b| b.start_line <= line)
            .checked_sub(1)?;
        let block = &self.blocks[idx];
        (block.end_line < line).then_some(block)
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

/// As the block sitting above a candidate seam: can it extend downward across
/// a blank line if the text below changes? Lists continue through blanks to
/// the next sufficiently-indented line, indented code bridges blank runs, and
/// footnote definition bodies do both; a closed fence/HTML block cannot
/// re-open, and everything else needs adjacency (which the blank breaks).
fn open_above_gap(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::List | BlockKind::IndentedCode | BlockKind::FootnoteDef
    )
}

/// As the last block of a two-sided window: can it absorb text below across
/// the trailing blank seam? Lists and indented code reach through blanks;
/// fenced code and HTML are listed defensively (an unclosed one already fails
/// the unowned-seam invariant by running into the blank).
fn open_below(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::List
            | BlockKind::IndentedCode
            | BlockKind::FencedCode
            | BlockKind::Html
            | BlockKind::FootnoteDef
    )
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
        if !line.trim_start().starts_with('[') {
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

/// The linchpin of the incremental scheme: after EVERY random edit, the
/// incrementally-maintained index must equal a fresh whole-document parse —
/// blocks (all fields), the definition registry, and the flat defs string.
/// An anti-vacuous engagement assertion proves the windowed path actually
/// runs (a fallback-always implementation must fail this suite).
#[cfg(test)]
mod fuzz {
    use super::*;
    use crate::line_index::LineIndex;
    use crate::testdoc::{self, XorShift};

    /// Mirrors the app's per-edit pipeline: mutate the rope, update the line
    /// index, then the block index — all with the same pre-mutation impact
    /// contract `App::edit_impact` uses.
    struct Harness {
        rope: Rope,
        lines: LineIndex,
        index: BlockIndex,
        version: u64,
    }

    impl Harness {
        fn new(src: &str) -> Self {
            let rope = Rope::from_str(src);
            let lines = LineIndex::build(&rope);
            let mut index = BlockIndex::default();
            index.set_full_floor_bytes(0); // force the windowed path
            index.rebuild_full(&rope, 0);
            Self {
                rope,
                lines,
                index,
                version: 0,
            }
        }

        fn splice(&mut self, start: usize, end: usize, text: &str) {
            let start_line = self.rope.byte_to_line(start);
            let end_line = self.rope.byte_to_line(end);
            let old = end_line - start_line + 1;
            self.rope
                .remove(self.rope.byte_to_char(start)..self.rope.byte_to_char(end));
            self.rope.insert(self.rope.byte_to_char(start), text);
            // Post-mutation count, like `App::mark_edited` (covers every line
            // break ropey recognizes, not just `\n`).
            let inserted_end = (start + text.len()).min(self.rope.len_bytes());
            let new = self.rope.byte_to_line(inserted_end) - start_line + 1;
            self.version += 1;
            let update = self.lines.apply_edit(&self.rope, start_line, old, new);
            let span = EditSpan {
                start_line,
                old_line_count: old,
                new_line_count: new,
                byte_delta: text.len() as isize - (end - start) as isize,
            };
            self.index
                .apply_edit(&self.rope, span, &update, &self.lines, self.version);
        }

        fn assert_matches_oracle(&self, context: &str) {
            assert!(
                self.index.is_current(self.version),
                "index fell out of sequence ({context})"
            );
            let mut oracle = BlockIndex::default();
            oracle.rebuild_full(&self.rope, self.version);
            assert_eq!(
                self.index.blocks(),
                oracle.blocks(),
                "blocks ({context})\ndoc:\n{}",
                self.rope
            );
            assert_eq!(
                self.index.defs(),
                oracle.defs(),
                "definition registry ({context})"
            );
            assert_eq!(
                self.index.ref_defs(),
                oracle.ref_defs(),
                "flat defs ({context})"
            );
            // Lookup coherence at every block edge.
            for (i, block) in self.index.blocks().iter().enumerate() {
                assert_eq!(
                    self.index.block_at_line(block.start_line),
                    Some(i),
                    "start lookup ({context})"
                );
                assert_eq!(
                    self.index.block_at_line(block.end_line),
                    Some(i),
                    "end lookup ({context})"
                );
            }
        }
    }

    /// Blank-separated prose and structure with no definitions or footnotes —
    /// the shapes where the windowed path must engage.
    fn engagement_doc(rng: &mut XorShift, blocks: usize) -> String {
        const FRAGMENTS: [&str; 8] = [
            "# Section heading\n",
            "A paragraph of plain prose with **bold** and `code`.\nIt continues on a second line.\n",
            "- alpha\n- beta\n- gamma\n",
            "1. first\n2. second\n",
            "```rust\nlet x = 1;\nlet y = x * 2;\n```\n",
            "> a quoted thought\n> across two lines\n",
            "| a | b |\n| - | - |\n| 1 | 2 |\n",
            "---\n",
        ];
        let mut out = String::new();
        for _ in 0..blocks {
            out.push_str(rng.pick(&FRAGMENTS));
            out.push('\n');
        }
        out
    }

    /// A weighted random edit, biased toward markdown-significant mutations.
    fn random_op(rng: &mut XorShift, rope: &Rope) -> (usize, usize, String) {
        match rng.below(10) {
            0..=5 => testdoc::random_edit(rng, rope),
            6 => {
                // Insert a structural piece at a random line start.
                const PIECES: [&str; 8] = [
                    "# ", "- ", "> ", "```\n", "~~~\n", "    ", "===\n", "| - |\n",
                ];
                let line = rng.below(rope.len_lines());
                let at = rope.line_to_byte(line);
                (at, at, rng.pick(&PIECES).to_string())
            }
            7 => {
                // Delete one whole line.
                let line = rng.below(rope.len_lines());
                let start = rope.line_to_byte(line);
                let end = if line + 1 < rope.len_lines() {
                    rope.line_to_byte(line + 1)
                } else {
                    rope.len_bytes()
                };
                (start, end, String::new())
            }
            8 => {
                // Split a block: blank line mid-document.
                let at = rope.char_to_byte(rng.below(rope.len_chars() + 1));
                (at, at, "\n\n".to_string())
            }
            _ => {
                // Merge blocks: delete the first blank line after a random
                // point, if any.
                let from = rng.below(rope.len_lines());
                for line in from..rope.len_lines() {
                    if rope.line(line).chars().all(char::is_whitespace)
                        && rope.line(line).len_bytes() > 0
                    {
                        let start = rope.line_to_byte(line);
                        let end = if line + 1 < rope.len_lines() {
                            rope.line_to_byte(line + 1)
                        } else {
                            rope.len_bytes()
                        };
                        return (start, end, String::new());
                    }
                }
                testdoc::random_edit(rng, rope)
            }
        }
    }

    #[test]
    fn windowed_reparse_matches_the_oracle_after_every_edit() {
        for seed in [0x5EA7_u64, 42] {
            let mut rng = XorShift::new(seed);
            let src = engagement_doc(&mut rng, 40);
            let mut h = Harness::new(&src);
            let ops = 150;
            for step in 0..ops {
                let (start, end, text) = random_op(&mut rng, &h.rope);
                h.splice(start, end, &text);
                h.assert_matches_oracle(&format!("engagement seed {seed:#x} step {step}"));
            }
            let stats = h.index.stats();
            eprintln!(
                "engagement seed {seed:#x}: {} windowed, {} full ({} fallbacks) over {ops} ops",
                stats.windowed_updates, stats.full_rebuilds, stats.full_fallbacks
            );
            assert!(
                stats.windowed_updates * 2 >= ops as usize,
                "windowed path must engage on definition-free documents: \
                 {} of {ops} ops (full {}, fallbacks {})",
                stats.windowed_updates,
                stats.full_rebuilds,
                stats.full_fallbacks
            );
        }
    }

    /// Extended soak, not a CI test:
    /// `cargo test --release soak -- --ignored --nocapture`
    #[test]
    #[ignore = "manual soak; run with --release --ignored --nocapture"]
    fn soak_thousands_of_edits_stay_oracle_exact() {
        for seed in 0xBEEF..0xBEEF + 4u64 {
            let mut rng = XorShift::new(seed);
            let src = engagement_doc(&mut rng, 400);
            let mut h = Harness::new(&src);
            for step in 0..2_000 {
                let (start, end, text) = random_op(&mut rng, &h.rope);
                h.splice(start, end, &text);
                h.assert_matches_oracle(&format!("soak seed {seed:#x} step {step}"));
            }
            let stats = h.index.stats();
            eprintln!(
                "soak seed {seed:#x}: {} windowed, {} full ({} grown, {} fallbacks)",
                stats.windowed_updates,
                stats.full_rebuilds,
                stats.windows_grown,
                stats.full_fallbacks
            );
        }
    }

    #[test]
    fn hazard_documents_stay_correct_under_random_edits() {
        let mut docs = vec![
            testdoc::many_blocks(30), // defs + footnotes → fallback-heavy
            testdoc::cjk_emoji(40),
            "para one\r\nsecond line\r\n\r\n- item\r\n".to_string(), // CRLF
            "open fence below\n\n```\nnever closed\nstill code\n".to_string(),
            "Setext title\n===\n\nbody\n\nAnother\n---\n".to_string(),
        ];
        match std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/sample.md")) {
            Ok(sample) => docs.push(sample),
            Err(_) => eprintln!("fixtures/sample.md missing; skipping that corpus doc"),
        }

        for (doc_idx, src) in docs.iter().enumerate() {
            let mut rng = XorShift::new(0xD0C_0000 + doc_idx as u64);
            let mut h = Harness::new(src);
            for step in 0..80 {
                let (start, end, text) = random_op(&mut rng, &h.rope);
                h.splice(start, end, &text);
                h.assert_matches_oracle(&format!("hazard doc {doc_idx} step {step}"));
            }
        }
    }

    #[test]
    fn fence_typed_character_by_character_stays_equivalent() {
        let mut h = Harness::new(
            "para above\n\ntarget line\n\npara below\n\n# heading\n\nlast paragraph\n",
        );
        let at = h.rope.line_to_byte(2);
        for i in 0..4 {
            h.splice(at + i, at + i, "`");
            h.assert_matches_oracle(&format!("backtick {}", i + 1));
        }
        // And remove them again (undo-like).
        for i in (0..4).rev() {
            h.splice(at + i, at + i + 1, "");
            h.assert_matches_oracle(&format!("removed backtick {i}"));
        }
    }

    #[test]
    fn list_continuation_across_blanks_never_splits_at_the_gap() {
        // Editing text below a list across a blank: the candidate seam between
        // them is excluded (lists reach down across gaps), so indenting the
        // paragraph absorbs it into the list — exactly like the full parse.
        let mut h = Harness::new("intro\n\n- item one\n- item two\n\ntail paragraph\n");
        let tail = h.rope.line_to_byte(5);
        h.splice(tail, tail, "    ");
        h.assert_matches_oracle("indented tail under a list");
        let blocks = h.index.blocks();
        assert!(
            blocks
                .iter()
                .any(|b| b.kind == BlockKind::List && b.end_line >= 5),
            "the indented tail joins the list block: {blocks:?}"
        );
    }

    #[test]
    fn setext_and_table_edits_stay_equivalent() {
        let mut h = Harness::new("Title\n===\n\nbody text\n\n| a | b |\n| - | - |\n| 1 | 2 |\n");
        // Break the setext underline (the heading degrades to a paragraph).
        let underline = h.rope.line_to_byte(1);
        h.splice(underline, underline, "x");
        h.assert_matches_oracle("broken setext underline");
        // Break the table delimiter row.
        let delim = h.rope.line_to_byte(6);
        h.splice(delim, delim + 1, "x");
        h.assert_matches_oracle("broken table delimiter");
    }

    #[test]
    fn definition_and_footnote_edits_take_the_full_path_and_stay_correct() {
        let mut h = Harness::new(
            "Use [a][r] and a note[^n].\n\n[r]: https://example.com\n\n[^n]: note body\n\ntail\n",
        );
        let full_before = h.index.stats().full_rebuilds;
        // Edit the definition's URL.
        let def_line = h.rope.line_to_byte(2);
        h.splice(def_line + 4, def_line + 4, "x");
        h.assert_matches_oracle("edited definition");
        assert!(
            h.index.stats().full_rebuilds > full_before,
            "definition edits must take the full path"
        );
        // Type a new footnote reference into the tail.
        let tail = h.rope.line_to_byte(6);
        for (i, ch) in "[^n]".char_indices() {
            h.splice(tail + i, tail + i, &ch.to_string());
            h.assert_matches_oracle(&format!("typing footnote ref byte {i}"));
        }
    }
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
