# Changelog

## [0.2.0] - 2026-09-08

### Added

- Command palette (`F1` / `Ctrl+Shift+P`), document outline (`F2` /
  `Ctrl+Shift+O`), and Save As shortcut (`F12` / `Ctrl+Shift+S`).
- Formatting actions for inline styles, links, headings, quotes, lists, tasks,
  and table alignment. Rendered task checkboxes can be toggled with the mouse.
- Footnote navigation and confirmation before opening HTTP(S) links.
- On-demand diagnostics for unmatched fences, unresolved references, malformed
  tables, and duplicate headings.
- New, open, recent, rename, and move-to-trash file actions. Recent files retain
  cursor and per-view scroll positions.
- Recovery snapshots for idle unsaved edits, with restore or discard on open.
- External file change detection. Manual save offers reload, overwrite, Save As,
  or cancel; autosave pauses when a conflict is detected.
- Optional word, character, selection, and reading-time statistics.
- Named theme families with matching syntax themes and terminal background
  detection. Settings can be changed in-app and saved with **Save defaults**.
- Render-mode stdin (`-`), plain-text wrapping with `--width`, and HTML output
  with `--html`.

### Changed

- Find supports case-sensitive, whole-word, regex, and selection-only modes,
  with Unicode-aware matching and cached results. Cancel restores the cursor
  and selection.
- Focus, raw, and read views keep separate scroll positions. Leaving read mode
  restores the editing cursor.
- Rendering builds rows for the viewport and caches blocks by content. Edits
  reparse nearby blocks when possible, falling back to a full parse when needed.
- Terminal pastes insert text verbatim without auto-pairing or list continuation.
- Configuration validation reports unknown themes, color keys, invalid colors,
  and invalid numeric values.

### Fixed

- Save failures remain visible; autosave backs off after errors and retries.
  Failed Save As leaves the original document path intact.
- Terminal cleanup attempts every restoration step. On Unix, SIGTERM and SIGHUP
  also trigger cleanup.
- Installers require checksum verification unless `MARQI_ALLOW_UNVERIFIED=1`
  is explicitly set.
- Render output strips terminal control characters and handles closed pipes.
  Render width and blockquote nesting are bounded.
- Fixed large Unicode grapheme handling, line-break mismatches between the
  editor and parser, and repeated scanning of emphasis and backtick runs.

## [0.1.0] - 2026-06-10

First public release.

### Added

- Live focus-preview editing: the block under the cursor stays editable as raw
  Markdown source while every other block renders as styled GitHub Flavored
  Markdown. Block partitioning derives from a single comrak parse, so setext
  headings, indented code, HTML blocks, and loose lists group exactly as they
  render.
- GFM support via comrak: headings, emphasis, strikethrough, lists, task lists,
  block quotes, pipe tables, fenced code blocks, autolinks, and footnotes
  (references show their source label, matching the `[name]:` definition).
- Syntax-highlighted fenced code blocks via syntect, with a render cache keyed
  by block content.
- Raw view (`^R`): every line shown as highlighted source with no markers
  stripped, while staying fully editable. Fenced-code interiors highlight as
  code, not as markdown.
- Full rendered-preview (read) mode (`^P`) and a `--render`/`-r` flag for
  non-TTY output.
- Unicode-aware editing: grapheme-cluster cursor motion and deletion,
  CJK/emoji-aware display widths, and soft wrapping.
- Four keybinding presets — Standard, Vim, Nano, and Emacs — cycled in-app with
  `^L`. Global commands use Ctrl shortcuts (`^G` help, `^P` preview, `^R` raw
  view, `^T` table mode, `^B` cursor shape, `^S` save, `^Q` quit), which
  terminals deliver more reliably than function keys. Standard gets
  Ctrl+arrow word motions; Emacs gets a working `C-Space` mark, `M-f`/`M-b`,
  and `C-x C-s`/`C-x C-c` chords. `Cmd` is accepted as a `Ctrl` alias in
  terminals that report it (kitty keyboard protocol).
- Find & replace: incremental smart-case find (`^F`; `/` plus `n`/`N` in Vim,
  `^W` in Nano, `M-s` in Emacs) with a match counter and wrapping navigation;
  `Tab` switches to replace, `Enter` replaces and advances, and `^A` replaces
  all occurrences as a single undo step.
- Mouse support: click to place the cursor (clicking a preview block opens it
  as source), drag to select, and wheel scrolling that detaches the viewport
  from the cursor until the next keypress.
- Optional auto-save (`editor.auto_save`): named buffers save automatically
  about two seconds after the last edit.
- Undo/redo with typing-burst coalescing, a bounded history, and a modified
  flag that clears when undo returns the buffer to its last-saved state. No-op
  edits never disturb the redo stack.
- Quitting with unsaved changes asks for confirmation (discard, cancel, or
  write first).
- Atomic, durable saves: the buffer streams to a sibling temp file (created
  with the target's permissions), is fsynced, and renamed over the target —
  a crash or full disk mid-write can never truncate or corrupt the file.
  Saving through a symlink updates the target and keeps the link; Windows
  retries renames that collide with antivirus/indexer locks.
- "Save as" prompt when saving an unnamed buffer (started empty or piped from
  stdin): pre-filled with `.md`, `~` expansion, an overwrite confirmation, and
  no path binding on a failed write.
- Smart Markdown editing, Obsidian-flavoured: Enter continues bullets, GFM
  task items, and numbered lists (numbers renumber to stay sequential, even in
  loose lists); an empty item exits the list; pair characters auto-close and
  wrap selections. List continuation never fires inside fenced code blocks.
- GFM pipe-table row mode (`^T`): cell navigation (Tab/Shift+Tab incl.
  BackTab terminals), row insert/delete, and row moves — indentation-aware and
  safe at end-of-file without a trailing newline.
- Clipboard integration: system clipboard (including Wayland) with an internal
  register fallback, and OSC 52 emission — tmux-passthrough-wrapped — whenever
  the system clipboard is missing or fails (e.g. SSH sessions). On Linux,
  OSC 52 is emitted alongside the system clipboard so copies survive after
  marqi exits (X11/Wayland clipboards die with the owning process).
- CRLF files keep their line endings: Enter and structural edits insert the
  file's dominant ending, and renumbering preserves terminators byte-exact.
- Configurable line-number gutter (off / absolute / relative).
- Configurable colors with dark/light variants, terminal auto-detection, and a
  truecolor-to-256-color downgrade that picks the closest of the colour cube or
  the grey ramp for limited terminals.
- Optional `editor.soft_break` setting: `"space"` (markdown reflow, the
  default) or `"break"` to render a single source newline as a visible line
  break (Obsidian-style).
- TOML configuration loaded from the platform config directory (or `-c DIR`),
  with warnings for unknown keys, unrecognised values, and a missing explicit
  config — typos never pass silently.
- Scrollable in-app help (`^G`) that stays usable on short and narrow
  terminals.
- A minimal status bar: a color-coded mode badge, the file name with a `[+]`
  modified flag, the cursor position, and just the three hints worth showing
  (`^G help · ^S save · ^Q quit`) — everything else lives in the help overlay.
- Terminal-safety guarantees: a panic hook and every error path restore the
  terminal (no leaked raw mode), and running with stdout redirected exits with
  a clear message instead of freezing.
- One-line installers: `install.sh` (Linux/macOS) and `install.ps1` (Windows)
  download the latest release for the current platform, verify its sha256
  checksum, install it, and add the install directory to `PATH` (shell profile
  on Unix, user environment on Windows). `MARQI_VERSION`, `MARQI_INSTALL_DIR`,
  and `MARQI_NO_MODIFY_PATH` are respected.

[0.1.0]: https://github.com/i-naji/marqi/releases/tag/v0.1.0
[0.2.0]: https://github.com/i-naji/marqi/compare/v0.1.0...v0.2.0
