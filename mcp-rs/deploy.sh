#!/bin/bash
# embedded-debug-mcp (Rust) 部署脚本
# 编译 + 安装到 ~/.local/bin/

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${HOME}/.local/bin"
BINARY_NAME="embedded-debug-mcp"

echo "=== embedded-debug-mcp Rust 部署 ==="
echo ""

# 1. 编译
echo "[1/3] 编译 release..."
cd "$SCRIPT_DIR"
cargo build --release
echo "  OK: target/release/${BINARY_NAME}"

# 2. 安装
echo "[2/3] 安装到 ${INSTALL_DIR}/..."
mkdir -p "$INSTALL_DIR"
cp "target/release/${BINARY_NAME}" "${INSTALL_DIR}/"
echo "  OK: $(ls -lh ${INSTALL_DIR}/${BINARY_NAME} | awk '{print $5}')"

# 3. 验证
echo "[3/3] 验证..."
"${INSTALL_DIR}/${BINARY_NAME}" --version

echo ""
echo "=== 部署完成 ==="
echo ""
echo "在项目 .mcp.json 中使用:"
echo '  "embedded-debug": {'
echo '    "command": "'"${INSTALL_DIR}/${BINARY_NAME}"'",'
echo '    "cwd": "'"$(pwd)"'",'
echo '    "env": { "RUST_LOG": "info" }'
echo '  }'
