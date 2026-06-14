#!/bin/bash
# Unified deploy: build + install binary + sync hooks + restart MCP
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT="debug-console-mcp"

echo "=== $PROJECT Unified Deploy ==="

# 1. Build
echo "[1/5] Building release..."
cd "$SCRIPT_DIR/mcp-rs"
cargo build --release --locked
echo "  OK: target/release/$PROJECT"

# 2. Install binaries (serial-sim is for testing only — NOT deployed)
echo "[2/5] Installing binaries..."
cp -f "target/release/$PROJECT" "$HOME/.local/bin/"
cp -f "target/release/dutabo" "$HOME/.local/bin/"
echo "  OK: debug-console-mcp ($(ls -lh $HOME/.local/bin/$PROJECT | awk '{print $5}'))"
echo "  OK: dutabo               ($(ls -lh $HOME/.local/bin/dutabo | awk '{print $5}'))"

# 3. Sync hooks (skip if source == dest, chezmoi manages hooks at source path)
echo "[3/5] Syncing hooks..."
HOOK_SRC="$SCRIPT_DIR/hooks/claude"
HOOK_DST="$HOME/.local/share/debug-console-mcp/hooks/claude"
if [ "$(realpath "$HOOK_SRC" 2>/dev/null || echo "$HOOK_SRC")" != "$(realpath "$HOOK_DST" 2>/dev/null || echo "$HOOK_DST")" ]; then
    mkdir -p "$HOOK_DST"
    for f in session-start.py session-stop.py user-prompt-submit.py statusline.py pre-tool-use.py lib.py; do
        if [ -f "$HOOK_SRC/$f" ]; then
            cp -f "$HOOK_SRC/$f" "$HOOK_DST/$f"
            echo "  $f → $HOOK_DST/"
        fi
    done
else
    echo "  SKIP: hooks managed at source ($HOOK_SRC)"
fi

# 4. Restart MCP
echo "[4/5] Restarting MCP..."
pkill -f "$PROJECT" 2>/dev/null || true
sleep 1

# 5. Verify
echo "[5/5] Verifying..."
"$HOME/.local/bin/$PROJECT" --version

echo ""
echo "=== Deploy complete ==="
echo "Binary: $HOME/.local/bin/$PROJECT"
echo "Hooks:  $HOOK_DST/"
echo ""
echo "Restart Claude Code or run 'systemctl restart --user claude' to apply."
