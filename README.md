# Marqi [![CI](https://github.com/i-naji/marqi/actions/workflows/ci.yml/badge.svg)](https://github.com/i-naji/marqi/actions/workflows/ci.yml) [![Release](https://img.shields.io/github/v/release/i-naji/marqi?sort=semver)](https://github.com/i-naji/marqi/releases) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

<img src="https://vhs.charm.sh/vhs-44euONSCrs1dAy6AmgbX6l.gif" alt="Made with VHS">

> **The terminal Markdown editor with true live preview.**
>
> **No split panes. No browser. Just write.**

## One document. One cursor.
Most Markdown editors make you switch between source and preview.

Marqi doesn't.

The document stays rendered while you write, and only the content under your cursor becomes editable.

**The preview is the editor.**

## Features

- ⚡ True in-place live preview
- 📝 GitHub Flavored Markdown
- 🎨 Syntax-highlighted code blocks
- 📋 Tables, task lists and footnotes
- ⌨️ Standard, Vim, Nano and Emacs keybindings
- 🖱️ Full mouse support
- 💾 Atomic saves with optional autosave
- 🎨 Configurable themes
- 📦 Single portable binary
- 🖥️ Linux, macOS and Windows

## Install

### Linux / macOS

```sh
curl -fsSL https://raw.githubusercontent.com/i-Naji/Marqi/main/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/i-Naji/Marqi/main/install.ps1 | iex
```

### From source

```sh
cargo install --git https://github.com/i-Naji/Marqi
```

## Quick Start

```sh
marqi README.md      # Edit a Markdown file
marqi -r README.md   # Render to stdout
marqi --help         # Show all commands and options
```

Press `Ctrl+G` inside Marqi to view the complete keybinding reference.

## Configuration

Optional. Marqi reads `config.toml` from the platform config directory
(`~/.config/marqi/` on Linux, `~/Library/Application Support/marqi/` on macOS,
`%APPDATA%\marqi\config\` on Windows), or from a path passed with `-c` — either
a TOML file directly (`marqi -c my-config.toml`) or a directory containing
`config.toml`.

```toml
[editor]
keybindings = "standard"   # "standard" | "vim" | "nano" | "emacs"
line_numbers = "off"       # "off" | "absolute" | "relative"
left_margin = 1            # blank columns left of the text (0 disables)
auto_save = false          # save ~2s after you stop typing

[theme]
variant = "auto"           # "auto" | "dark" | "light"
```

See [`marqi.example.toml`](marqi.example.toml) for all available options.

## Documentation

* [`ARCHITECTURE.md`](ARCHITECTURE.md)
* [`CHANGELOG.md`](CHANGELOG.md)

## License

[MIT](LICENSE)
