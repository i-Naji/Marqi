# Architecture

Marqi is a single-binary terminal Markdown editor. This document maps the code
and explains the rendering model, so contributors can find their way around and
so the planned **vault** (multi-note workspace) can be added without a rewrite.

## Crates and the event loop

`main.rs` parses the CLI, loads the buffer (from a file, piped stdin, or empty),
loads config, then runs a render-on-event loop:

```
draw(frame) ──▶ event::read() (blocks) ──▶ app.handle_key() ──▶ draw …
```

`tui.rs` owns terminal setup/teardown. It enters raw mode + the alternate
screen, opts into the CSI-u keyboard protocol where supported (so `Shift`
+movement is reported unambiguously), and installs a panic hook that restores
the terminal before any panic message prints. Restoration is idempotent, so the
terminal is never left broken.

## Module map

| Module | Responsibility |
| --- | --- |
| `main.rs` | CLI parsing, stdin/file input, terminal lifecycle, event loop, `--render` |
| `tui.rs` | Raw mode / alternate screen, keyboard-enhancement flags, panic-safe restore |
| `buffer.rs` | `TextBuffer`: a `ropey::Rope` plus its path and modified flag; load/save/slice |
| `text.rs` | Grapheme-cluster boundary lookups over a rope (`next_grapheme`/`prev_grapheme`) |
| `cursor.rs` | Byte-offset cursor with a sticky goal column; grapheme/word/line/page motions |
| `layout.rs` | Display geometry: exact per-line row counts with chunked prefix sums; visual rows materialize on demand through a bounded cache; byte ↔ (row, col) mapping |
| `line_index.rs` | Per-line incremental metadata: tokenizer fence state + class flags (blank / footnote / definition-shaped), updated in lockstep with every edit |
| `block_index.rs` | The Markdown block index: comrak block boundaries + link-reference definitions, maintained per edit by windowed reparsing with a full-reparse fallback |
| `history.rs` | Undo/redo as reversible edits with typing-burst coalescing |
| `clipboard.rs` | System clipboard + internal register fallback + OSC 52 for SSH |
| `color.rs` | Truecolor detection, RGB → xterm-256 downgrade, `#rrggbb` parsing |
| `config.rs` | TOML configuration loading from the platform config dir |
| `app.rs` | `App`: editor state, input handling per preset/mode, editing primitives, cache orchestration |
| `app/prompt.rs` | Bottom-line prompts: save-as, overwrite and quit confirms, shared line editing |
| `app/search.rs` | Find & replace: smart-case matching, match stepping, single-step replace-all |
| `app/smart_edit.rs` | Auto-paired delimiters and list-marker continuation on `Enter` |
| `app/table.rs` | GFM pipe-table row mode (cell jumps, row insert/delete/move) |
| `view.rs` | Row indexes for the hybrid/raw/read views (segments + prefix sums, no stored rows); viewport assembly; the LRU block render cache and height memo |
| `ui.rs` | ratatui drawing: editor/preview/help/status/gutter and hardware-cursor placement |
| `markdown/preview.rs` | comrak AST → styled lines with markers stripped (preview blocks) |
| `markdown/tokenizer.rs` | Lossless per-byte styling of the raw active block |
| `markdown/highlight.rs` | syntect highlighting for fenced code blocks |
| `markdown/theme.rs` | Element palettes (dark/light), terminal variant detection, config overrides |

## The rendering model (focus mode)

The document is partitioned into top-level blocks by line. The block the cursor
sits in is rendered **raw** — its exact source rows, styled by the lossless
tokenizer — while every other block is rendered as **preview** (markers
stripped, headings/tables/code laid out).

The key invariant: the cursor's byte ↔ column mapping always comes from the full
raw `Layout`, independent of how tall a preview block renders. The active block
reproduces its source bytes exactly (the tokenizer only assigns a style per byte
and never rewrites text), so the cursor maps precisely even though the
surrounding document is reflowed.

```
            ┌── other blocks: preview (cached by content hash) ──┐
  Layout ──▶│  # Heading            ◆ Heading                    │
 (raw rows) │  cursor's block  ──▶  - one **raw** with markers   │ ◀─ active, raw
            │  more text            rendered preview again       │
            └────────────────────────────────────────────────────┘
```

## State, versioning, and caches

The engine keeps the rope as the only full-document store; every derived
structure is an *index* (cheap per-line/per-block metadata) with the expensive
artifacts — display cells, styled rows — materialized on demand for the
viewport and bounded by caches.

Every buffer mutation goes through one path in `app.rs`
(`replace_range` → `mark_edited`), which:

1. bumps a monotonic `version`,
2. recounts the display rows of the touched lines only (`Layout` keeps exact
   per-line row counts + chunked prefix sums; cells are not stored),
3. updates the per-line `LineIndex` (fence state ripples forward only until it
   re-converges) and
4. feeds the edit to the `BlockIndex`, which usually reparses just a window
   between two sound blank-line seams (suffix blocks are reused, shifted) and
   falls back to a whole-document comrak parse whenever safety cannot be
   proven — footnote- or definition-shaped lines in reach, no seam found, or a
   document under 64KB. A version-mismatch backstop in `ensure_blocks` means a
   missed update degrades to a full reparse, never to a stale index.

The views are row *indexes*, not row stores: `HybridView`/`PreviewView` hold
document-ordered segments (raw runs, preview blocks, the active block) with
prefix-summed row counts. Rendered rows exist only for the window
`view::assemble` is asked for — selection and the active-line background are
applied there, so cursor and selection changes never rebuild any structure.
Raw rows tokenize per window, seeded from the `LineIndex` fence state, which
is byte-identical to a whole-document scan. Inactive block heights come from a
content-keyed memo (bucketed by wrap width, current + previous retained), so
the row math never re-renders an evicted block.

`view::ViewCache` memoizes rendered preview blocks keyed by (content hash,
ref-defs hash, width, theme flags) under a byte budget with LRU eviction —
entries touched by the current frame are pinned, cold ones evict oldest-first.
`markdown::CodeHighlighter` memoizes syntect output keyed by (lang, code,
width). Set `MARQI_STATS=1` for a status-bar counter segment and an exit dump
(parse/build/assemble timings, windowed vs full reparses, cache occupancy).

Equivalence is enforced by tests at every layer: the legacy full-document
builders survive as `#[cfg(test)]` oracles (`view::build_full`,
`render_preview_cached`, eager layout), and fuzz harnesses assert after every
random edit that the incremental `Layout`/`LineIndex`/`BlockIndex` equal a
fresh build — plus an anti-vacuous check that the windowed path actually
engages.

## Vault extension plan (not yet implemented)

Marqi is deliberately structured so an Obsidian-style **vault** — a workspace of
many notes with `[[wikilinks]]`, backlinks, and a file switcher — can be layered
on without disturbing the editing core. Today `App` owns exactly one
`TextBuffer`. The seam is to split per-document state out of the shared,
document-independent services:

- **Extract a `Document`** owning the per-note state that already clusters
  together in `App`: `buffer`, `cursor`, `selection_anchor`, `history`,
  `scroll_y`, the `layout` (+ `layout_width`/`layout_dirty`), `line_index`,
  the view caches (`view`, `view_cache`, `view_version`, …), and `version`.
- **Keep on `App`** the services that are not per-document: `theme`,
  `highlighter`, `clipboard`, `preset`, `mode`, `line_numbers`, `tab_width`,
  `scrolloff`, and the editor-area geometry. `App` then holds the *active*
  `Document` (and later a list of open ones).
- **Add a `Vault`/`Workspace`** owning a root directory, a note index, the set
  of open `Document`s (tabs / MRU), and a link graph for backlinks.

Supporting pieces, all of which compose with the current code:

- **Wikilinks**: comrak exposes `extension.wikilinks_title_after_pipe` /
  `wikilinks_title_before_pipe`; enable them in `markdown::gfm_options` behind a
  vault flag and add a resolver mapping `[[Note]]` → a path in the vault.
- **Backlinks**: build an index by scanning the vault for links into each note;
  invalidate per-note on save.
- **UI**: the ratatui layout already splits the screen into regions, so a sidebar
  file tree slots in next to the editor; a quick-switcher overlay can reuse the
  existing help-overlay pattern in `ui.rs`; "follow link under cursor" becomes a
  new action in `app.rs`.

Because the per-document fields are already grouped and mutated through a single
path, the extraction is mechanical rather than invasive.

## Known limitations & future work

Block partitioning for both the hybrid view and the read-mode preview comes
from comrak (`block_index::parse_blocks`, applied to a window or the whole
document), so the two agree, there is one renderer to maintain, and constructs
like setext headings, indented code, HTML blocks, and loose lists are grouped
exactly as they render. The remaining deliberate simplifications:

- **The active-block tokenizer is a forgiving scanner, not full CommonMark.**
  `markdown::tokenizer` assigns one style per byte and never rewrites source, so
  it is always lossless. It styles inline links (including URLs with balanced
  parentheses) and honors GFM's intraword-`_` rule, but it does not resolve
  reference links (`[text][ref]`, which need the document's link-reference map)
  or implement the complete emphasis flanking algorithm. Rare cases may look
  slightly different raw vs. rendered.
- **Cross-block reference links resolve through a defs appendix.** The block
  index collects the document's link-reference definitions (one per gap line);
  each block's isolated re-parse gets them appended, and their hash is part of
  every render-cache key, so editing a definition re-renders everything that
  could mention it. Multi-line definitions (title on the following line) are
  not collected — same as the per-line probe always behaved.
- **Documents that use footnotes reparse in full on edits near them.** comrak
  drops an unreferenced footnote definition from the AST, so an edit touching
  footnote syntax can flip a *distant* definition between block and gap —
  invisible to any window. Such edits (and windows containing footnote syntax)
  take the whole-document path; everything else still windows.
- **Footnotes** render minimally: a reference shows its index (`[N]`) and the
  definition shows a `[name]:` label. There is no superscripting or
  reference↔definition navigation yet.
