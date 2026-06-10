# Marqi

[![CI](https://github.com/i-naji/marqi/actions/workflows/ci.yml/badge.svg)](https://github.com/i-naji/marqi/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/i-naji/marqi?sort=semver)](https://github.com/i-naji/marqi/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Marqi** is a Markdown editor for the terminal with a true live preview: the
document stays rendered while you type, and only the part you are editing
shows its source. No split panes, no browser.

- **Live preview, in place** — GitHub Flavored Markdown with syntax-highlighted
  code blocks, tables, task lists, and footnotes.
- **Your keybindings** — Standard, Vim, Nano, or Emacs presets, plus find &
  replace and full mouse support.
- **Safe by default** — atomic writes, optional auto-save, and a quit guard for
  unsaved changes.
- **Fast and portable** — a single small binary for Linux, macOS, and Windows.

## Install

Linux / macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/i-naji/marqi/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/i-naji/marqi/main/install.ps1 | iex
```

Both install the [latest release](https://github.com/i-naji/marqi/releases/latest)
and add it to your `PATH`. Or build from source:

```sh
cargo install --git https://github.com/i-naji/marqi
```

## Usage

```sh
marqi notes.md          # edit a file
curl -s URL | marqi     # edit Markdown piped from stdin
marqi -r notes.md       # render to stdout
```

Press `^G` inside the editor for the full keybinding reference, or run
`marqi --help` for the CLI options.

## Configuration

Optional. Marqi reads `config.toml` from the platform config directory
(`~/.config/marqi/` on Linux, `~/Library/Application Support/marqi/` on macOS,
`%APPDATA%\marqi\config\` on Windows), or from a directory passed with `-c`.

```toml
[editor]
keybindings = "standard"   # "standard" | "vim" | "nano" | "emacs"
line_numbers = "off"       # "off" | "absolute" | "relative"
auto_save = false          # save ~2s after you stop typing

[theme]
variant = "auto"           # "auto" | "dark" | "light"
```

See [`marqi.example.toml`](marqi.example.toml) for every option, including the
full colour palette.

## Development

```sh
cargo test && cargo clippy --all-targets --all-features -- -D warnings
```

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the rendering model and
[`CHANGELOG.md`](CHANGELOG.md) for history. Releases are published by pushing a
`v*` tag.

## License

[MIT](LICENSE)
