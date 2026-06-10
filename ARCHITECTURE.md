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
| `layout.rs` | Display layout: rope → wrapped visual rows, and the byte ↔ (row, col) mapping |
| `history.rs` | Undo/redo as reversible edits with typing-burst coalescing |
| `clipboard.rs` | System clipboard + internal register fallback + OSC 52 for SSH |
| `color.rs` | Truecolor detection, RGB → xterm-256 downgrade, `#rrggbb` parsing |
| `config.rs` | TOML configuration loading from the platform config dir |
| `app.rs` | `App`: editor state, input handling per preset/mode, editing primitives, cache orchestration |
| `app/prompt.rs` | Bottom-line prompts: save-as, overwrite and quit confirms, shared line editing |
| `app/search.rs` | Find & replace: smart-case matching, match stepping, single-step replace-all |
| `app/smart_edit.rs` | Auto-paired delimiters and list-marker continuation on `Enter` |
| `app/table.rs` | GFM pipe-table row mode (cell jumps, row insert/delete/move) |
| `view.rs` | The hybrid focus view + per-block render cache; cached full preview for read mode |
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

Every buffer mutation goes through one path in `app.rs`
(`replace_range` → `mark_edited`), which:

1. bumps a monotonic `version`,
2. incrementally patches the display `Layout` for the touched lines (falling back
   to a full rebuild on width/tab changes), and
3. marks the preview and hybrid view dirty.

`ensure_layout` / `ensure_view` / `ensure_preview` rebuild the relevant cache
only when its inputs (content `version`, wrap width, cursor block, selection)
changed. `view::ViewCache` memoizes rendered preview blocks keyed by a hash of
(source slice, width, heading-glyph flag), and `markdown::CodeHighlighter`
memoizes syntect output keyed by (lang, code, width). Both caches are bounded
and cleared wholesale when they exceed their byte/entry limits.

## Vault extension plan (not yet implemented)

Marqi is deliberately structured so an Obsidian-style **vault** — a workspace of
many notes with `[[wikilinks]]`, backlinks, and a file switcher — can be layered
on without disturbing the editing core. Today `App` owns exactly one
`TextBuffer`. The seam is to split per-document state out of the shared,
document-independent services:

- **Extract a `Document`** owning the per-note state that already clusters
  together in `App`: `buffer`, `cursor`, `selection_anchor`, `history`,
  `scroll_y`, the `layout` (+ `layout_width`/`layout_dirty`), the view caches
  (`view`, `view_cache`, `view_version`, …), and `version`.
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

Block partitioning for both the hybrid view and the read-mode preview comes from
a single comrak parse (`view::blocks_from_ast`), so the two agree, there is one
renderer to maintain, and constructs like setext headings, indented code, HTML
blocks, and loose lists are grouped exactly as they render. The remaining
deliberate simplifications:

- **The active-block tokenizer is a forgiving scanner, not full CommonMark.**
  `markdown::tokenizer` assigns one style per byte and never rewrites source, so
  it is always lossless. It styles inline links (including URLs with balanced
  parentheses) and honors GFM's intraword-`_` rule, but it does not resolve
  reference links (`[text][ref]`, which need the document's link-reference map)
  or implement the complete emphasis flanking algorithm. Rare cases may look
  slightly different raw vs. rendered.
- **Inactive blocks re-parse their own source slice on a cache miss.** This keeps
  the per-block render cache simple, but a link that depends on a reference
  definition in a *different* block is not resolved in the cached preview. The
  active block and the whole-document `marqi -r` render are unaffected; fully
  resolving cross-block references would mean threading comrak's reference map
  into the per-block renderer.
- **Footnotes** render minimally: a reference shows its index (`[N]`) and the
  definition shows a `[name]:` label. There is no superscripting or
  reference↔definition navigation yet.
