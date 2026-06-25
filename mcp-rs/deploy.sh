#!/bin/bash
# debug-console-mcp (Rust) deployment script
# Build + install to ~/.local/bin/

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${HOME}/.local/bin"
BINARY_NAME="debug-console-mcp"

echo "=== debug-console-mcp Rust deployment ==="
echo ""

# 1. Build (use --locked for reproducible builds from committed Cargo.lock)
echo "[1/3] Building release..."
cd "$SCRIPT_DIR"
cargo build --release --locked
echo "  OK: target/release/${BINARY_NAME}"

# 2. Install
echo "[2/3] Installing to ${INSTALL_DIR}/..."
mkdir -p "$INSTALL_DIR"
cp -f "target/release/${BINARY_NAME}" "${INSTALL_DIR}/"
echo "  OK: $(ls -lh ${INSTALL_DIR}/${BINARY_NAME} | awk '{print $5}')"

# 3. Verify
echo "[3/3] Verifying..."
"${INSTALL_DIR}/${BINARY_NAME}" --version

echo ""
echo "=== Deployment complete ==="
echo ""
echo "Use in project .mcp.json:"
echo '  "debug-console": {'
echo '    "command": "'"${INSTALL_DIR}/${BINARY_NAME}"'",'
echo '    "cwd": "'"$(pwd)"'",'
echo '    "env": { "RUST_LOG": "info" }'
echo '  }'
