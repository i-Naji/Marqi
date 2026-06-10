#!/bin/sh
# Marqi installer.
#
# Downloads the latest release binary for this machine, verifies its sha256
# checksum, and installs it:
#
#   curl -fsSL https://raw.githubusercontent.com/i-naji/marqi/main/install.sh | sh
#
# Environment overrides:
#   MARQI_VERSION       install a specific tag, e.g. v0.1.0 (default: latest)
#   MARQI_INSTALL_DIR   install directory (default: /usr/local/bin if writable,
#                       otherwise ~/.local/bin)

set -eu

REPO="i-naji/marqi"
BIN="marqi"

say() { printf '%s\n' "$*"; }
err() {
    printf 'install.sh: error: %s\n' "$*" >&2
    exit 1
}
have() { command -v "$1" >/dev/null 2>&1; }

download() { # url dest
    if have curl; then
        curl -fsSL "$1" -o "$2"
    elif have wget; then
        wget -qO "$2" "$1"
    else
        err "curl or wget is required"
    fi
}

fetch() { # url -> stdout
    if have curl; then
        curl -fsSL "$1"
    else
        wget -qO- "$1"
    fi
}

# --- platform -> release target triple ---
os=$(uname -s)
arch=$(uname -m)
case "$os" in
Linux)
    case "$arch" in
    x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
    aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
    *) err "unsupported architecture '$arch' — see https://github.com/$REPO/releases" ;;
    esac
    ;;
Darwin)
    case "$arch" in
    x86_64) target="x86_64-apple-darwin" ;;
    arm64) target="aarch64-apple-darwin" ;;
    *) err "unsupported architecture '$arch' — see https://github.com/$REPO/releases" ;;
    esac
    ;;
MINGW* | MSYS* | CYGWIN*)
    err "on Windows, download the .zip from https://github.com/$REPO/releases/latest"
    ;;
*)
    err "unsupported OS '$os' — see https://github.com/$REPO/releases"
    ;;
esac

# --- resolve the version ---
version="${MARQI_VERSION:-}"
if [ -z "$version" ]; then
    version=$(fetch "https://api.github.com/repos/$REPO/releases/latest" |
        grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/') || true
    [ -n "$version" ] || err "could not determine the latest release"
fi

archive="$BIN-$version-$target.tar.gz"
url="https://github.com/$REPO/releases/download/$version/$archive"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say "Downloading $BIN $version ($target)..."
download "$url" "$tmp/$archive"

# --- verify the checksum when a sha256 tool is available ---
if download "$url.sha256" "$tmp/$archive.sha256" 2>/dev/null; then
    if have sha256sum; then
        (cd "$tmp" && sha256sum -c "$archive.sha256" >/dev/null) ||
            err "checksum verification failed"
    elif have shasum; then
        (cd "$tmp" && shasum -a 256 -c "$archive.sha256" >/dev/null) ||
            err "checksum verification failed"
    else
        say "note: no sha256 tool found; skipping checksum verification"
    fi
else
    say "note: checksum file unavailable; skipping verification"
fi

tar -xzf "$tmp/$archive" -C "$tmp"
[ -f "$tmp/$BIN" ] || err "archive did not contain the $BIN binary"

# --- pick the install directory ---
dir="${MARQI_INSTALL_DIR:-}"
if [ -z "$dir" ]; then
    if [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
        dir=/usr/local/bin
    else
        dir="$HOME/.local/bin"
    fi
fi
mkdir -p "$dir"
install -m 755 "$tmp/$BIN" "$dir/$BIN"

say "Installed $("$dir/$BIN" -V) to $dir/$BIN"

# --- make sure the install directory is on PATH ---
case ":$PATH:" in
*":$dir:"*) exit 0 ;;
esac
if [ -n "${MARQI_NO_MODIFY_PATH:-}" ]; then
    say "note: $dir is not on your PATH — add:  export PATH=\"$dir:\$PATH\""
    exit 0
fi

shell_name=$(basename "${SHELL:-sh}")
if [ "$shell_name" = "fish" ]; then
    fish_conf="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d"
    mkdir -p "$fish_conf"
    if [ ! -f "$fish_conf/marqi.fish" ]; then
        printf 'fish_add_path "%s"\n' "$dir" >"$fish_conf/marqi.fish"
        say "Added $dir to your PATH via $fish_conf/marqi.fish"
    fi
else
    case "$shell_name" in
    zsh) profile="${ZDOTDIR:-$HOME}/.zshrc" ;;
    bash) profile="$HOME/.bashrc" ;;
    *) profile="$HOME/.profile" ;;
    esac
    export_line="export PATH=\"$dir:\$PATH\" # added by marqi installer"
    if ! grep -qsF -- "$export_line" "$profile"; then
        printf '\n%s\n' "$export_line" >>"$profile"
        say "Added $dir to your PATH in $profile"
    fi
fi
say "Restart your shell, or run:  export PATH=\"$dir:\$PATH\""
