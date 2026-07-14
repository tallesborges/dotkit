#!/usr/bin/env bash
#
# dotkit installer — downloads the prebuilt binary for your platform from the
# GitHub releases and installs it into a directory on your PATH. No Rust needed.
#
# Quick install:
#   curl -fsSL https://raw.githubusercontent.com/tallesborges/dotkit/master/install.sh | bash
#
# Options (when run locally, e.g. ./install.sh --version v0.1.0):
#   --version <tag>     install a specific release tag (default: latest)
#   --bin-dir <dir>     install into <dir> (default: ~/.local/bin)
#   --from-source       build from a local checkout with cargo instead of downloading
#
# Env overrides: VERSION, BIN_DIR
#
set -euo pipefail

REPO="tallesborges/dotkit"
BIN_NAME="dotkit"

VERSION="${VERSION:-}"
BIN_DIR="${BIN_DIR:-}"
FROM_SOURCE="0"

while [ $# -gt 0 ]; do
    case "$1" in
        --version) shift; VERSION="${1:-}" ;;
        --bin-dir) shift; BIN_DIR="${1:-}" ;;
        --from-source) FROM_SOURCE="1" ;;
        -h|--help)
            sed -n '2,15p' "${BASH_SOURCE[0]:-$0}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "dotkit install: unknown option '$1'" >&2; exit 2 ;;
    esac
    shift
done

# Pick a bin dir: prefer an existing one already on PATH, else ~/.local/bin.
if [ -z "$BIN_DIR" ]; then
    BIN_DIR="$HOME/.local/bin"
    for d in "$HOME/.local/bin" "$HOME/.cargo/bin" "$HOME/bin" /usr/local/bin; do
        case ":$PATH:" in
            *":$d:"*) [ -d "$d" ] && { BIN_DIR="$d"; break; } ;;
        esac
    done
fi
BIN_DIR="${BIN_DIR/#\~/$HOME}"

# ── Optional: build from a local checkout ────────────────────────────────────
if [ "$FROM_SOURCE" = "1" ]; then
    repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
    [ -f "$repo_dir/Cargo.toml" ] || { echo "dotkit install: --from-source must run from a dotkit checkout" >&2; exit 1; }
    command -v cargo >/dev/null 2>&1 || { echo "dotkit install: 'cargo' not found. Install Rust from https://rustup.rs" >&2; exit 1; }
    echo "==> Building dotkit (release) from $repo_dir…"
    cargo build --release --manifest-path "$repo_dir/Cargo.toml"
    bin_path="$repo_dir/target/release/$BIN_NAME"
else
    # ── Detect platform → release target triple ──────────────────────────────
    os="$(uname -s)"; arch="$(uname -m)"
    case "$os" in
        Darwin) os_part="apple-darwin" ;;
        Linux)  os_part="unknown-linux-gnu" ;;
        *) echo "dotkit install: unsupported OS '$os' — try --from-source" >&2; exit 1 ;;
    esac
    case "$arch" in
        arm64|aarch64) arch_part="aarch64" ;;
        x86_64|amd64)  arch_part="x86_64" ;;
        *) echo "dotkit install: unsupported arch '$arch' — try --from-source" >&2; exit 1 ;;
    esac
    target="${arch_part}-${os_part}"
    case "$target" in
        aarch64-apple-darwin|x86_64-unknown-linux-gnu) ;;
        *) echo "dotkit install: no prebuilt binary for '$target' — try --from-source" >&2; exit 1 ;;
    esac

    command -v curl >/dev/null 2>&1 || { echo "dotkit install: 'curl' is required" >&2; exit 1; }

    # ── Resolve version ──────────────────────────────────────────────────────
    if [ -z "$VERSION" ]; then
        echo "==> Resolving latest release…"
        latest_json="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")" \
            || { echo "dotkit install: could not reach GitHub releases API (set --version)" >&2; exit 1; }
        [[ "$latest_json" =~ \"tag_name\":[[:space:]]*\"([^\"]+)\" ]] && VERSION="${BASH_REMATCH[1]}"
        [ -n "$VERSION" ] || { echo "dotkit install: could not determine latest release (set --version)" >&2; exit 1; }
    fi
    echo "==> Version: $VERSION"

    asset="${BIN_NAME}-${VERSION}-${target}.tar.gz"
    base="https://github.com/$REPO/releases/download/$VERSION"
    tmp="$(mktemp -d)"

    echo "==> Downloading $asset…"
    curl -fsSL "$base/$asset" -o "$tmp/$asset" \
        || { rm -rf "$tmp"; echo "dotkit install: download failed: $base/$asset" >&2; exit 1; }

    # ── Verify checksum against the release SHA256SUMS (best-effort) ─────────
    if curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" 2>/dev/null; then
        expected="$(grep " $asset\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
        if [ -n "$expected" ]; then
            if command -v sha256sum >/dev/null 2>&1; then
                actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
            else
                actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
            fi
            [ "$actual" = "$expected" ] \
                || { rm -rf "$tmp"; echo "dotkit install: checksum mismatch for $asset" >&2; exit 1; }
            echo "==> Checksum OK"
        fi
    fi

    echo "==> Extracting…"
    tar -xzf "$tmp/$asset" -C "$tmp"
    found="$(find "$tmp" -type f -name "$BIN_NAME" -perm -u+x)"
    bin_path="${found%%$'\n'*}"
    [ -n "$bin_path" ] \
        || { rm -rf "$tmp"; echo "dotkit install: '$BIN_NAME' not found in the archive" >&2; exit 1; }
fi

# ── Install the binary into BIN_DIR ──────────────────────────────────────────
mkdir -p "$BIN_DIR"
install -m 0755 "$bin_path" "$BIN_DIR/$BIN_NAME" 2>/dev/null \
    || { cp -f "$bin_path" "$BIN_DIR/$BIN_NAME"; chmod 0755 "$BIN_DIR/$BIN_NAME"; }
[ "${tmp:-}" = "" ] || rm -rf "$tmp"

echo "==> Installed: $BIN_DIR/$BIN_NAME"
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        echo
        echo "Note: $BIN_DIR is not on your PATH. Add this to your shell profile:"
        echo "    export PATH=\"$BIN_DIR:\$PATH\""
        ;;
esac
echo
"$BIN_DIR/$BIN_NAME" --version 2>/dev/null || true
echo "Done. Run 'dotkit --help' to get started."
