#!/bin/bash
# ensure-binary.sh — fetch prebuilt binary from GitHub Release, verify, cache, exec.
#
# Usage: ensure-binary.sh <binary-name> [args...]
#
# Three-tier resolution (Claudix pattern):
#   1. Cached → exec directly from $CLAUDE_PLUGIN_DATA/bin/
#   2. Cache miss → download from GitHub Release, sha256 verify, cache, exec
#   3. Download fails → print instructions, suggest cargo install fallback
#
# Environment:
#   CLAUDE_PLUGIN_ROOT   — plugin install dir (set by Claude Code)
#   CLAUDE_PLUGIN_DATA   — persistent cache dir (survives plugin updates)

set -euo pipefail

BINARY_NAME="${1:-}"
shift || true

if [ -z "$BINARY_NAME" ]; then
    echo "Usage: ensure-binary.sh <binary-name> [args...]" >&2
    exit 1
fi

# ── Resolve paths ──────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd -- "$(dirname "$0")" && pwd)"
PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-$(cd -- "$SCRIPT_DIR/.." && pwd)}"
PLUGIN_DATA="${CLAUDE_PLUGIN_DATA:-$HOME/.claude/plugins/data/embedded-debug}"
CACHE_DIR="$PLUGIN_DATA/bin"

# Read version from plugin manifest
VERSION=$(grep -oP '"version"\s*:\s*"\K[^"]+' "$PLUGIN_ROOT/.claude-plugin/plugin.json" 2>/dev/null || echo "0.3.0")
REPO="bitshelf/debug-console-mcp"
BASE_URL="https://github.com/$REPO/releases/download/v${VERSION}"

# ── Platform detection ─────────────────────────────────────────────────────

ARCH=$(uname -m)
case "$ARCH" in
    x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
    aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
    *)       echo "Unsupported architecture: $ARCH. Use cargo install --git instead." >&2
             exit 1 ;;
esac

CACHED_BIN="$CACHE_DIR/$BINARY_NAME"

# ── Fast path: already cached ──────────────────────────────────────────────

if [ -x "$CACHED_BIN" ]; then
    exec "$CACHED_BIN" "$@"
fi

# ── Cache miss: download from GitHub Release ───────────────────────────────

DOWNLOAD_URL="${BASE_URL}/${BINARY_NAME}-${TARGET}"
CHECKSUM_URL="${BASE_URL}/SHA256SUMS"

echo "→ Downloading $BINARY_NAME v${VERSION} for ${TARGET}..." >&2
mkdir -p "$CACHE_DIR"

# Create a temp dir for atomic install
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

# Download binary
if ! curl -fsSL --connect-timeout 30 --retry 3 \
    -o "$TMP_DIR/$BINARY_NAME" \
    "$DOWNLOAD_URL"; then
    echo "✗ Failed to download $DOWNLOAD_URL" >&2
    echo "→ Fallback: install via cargo" >&2
    echo "  cargo install --git https://github.com/$REPO" >&2
    exit 1
fi

# Download checksums
if curl -fsSL --connect-timeout 10 --retry 2 \
    -o "$TMP_DIR/SHA256SUMS" \
    "$CHECKSUM_URL"; then
    # Verify
    EXPECTED=$(grep "${BINARY_NAME}-${TARGET}" "$TMP_DIR/SHA256SUMS" | awk '{print $1}')
    if [ -n "$EXPECTED" ]; then
        ACTUAL=$(sha256sum "$TMP_DIR/$BINARY_NAME" | awk '{print $1}')
        if [ "$EXPECTED" != "$ACTUAL" ]; then
            echo "✗ SHA256 mismatch!" >&2
            echo "  expected: $EXPECTED" >&2
            echo "  actual:   $ACTUAL" >&2
            exit 1
        fi
        echo "✓ SHA256 verified" >&2
    else
        echo "⚠ SHA256SUMS missing entry for ${BINARY_NAME}-${TARGET}, skipping verification" >&2
    fi
else
    echo "⚠ SHA256SUMS not available, skipping verification" >&2
fi

# Atomic install
chmod +x "$TMP_DIR/$BINARY_NAME"
mv "$TMP_DIR/$BINARY_NAME" "$CACHED_BIN"

echo "✓ Cached to $CACHED_BIN" >&2

exec "$CACHED_BIN" "$@"
