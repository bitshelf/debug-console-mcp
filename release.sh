#!/bin/bash
set -e
VERSION="${1:-$(grep '^version' mcp-rs/Cargo.toml | head -1 | cut -d'"' -f2)}"
echo "=== debug-console-mcp release v$VERSION ==="

echo "[1/4] Running tests..."
cd mcp-rs && cargo test --lib -- --test-threads=1 2>&1 | tail -3

echo "[2/4] Building release..."
cargo build --release --locked 2>&1 | tail -1

echo "[3/4] Deploying..."
bash ../deploy-all.sh 2>&1 | tail -3

echo "[4/4] Tagging..."
git tag -a "v$VERSION" -m "Release v$VERSION" 2>/dev/null || echo "  Tag exists, skipping"
echo "  Binary size: $(ls -lh target/release/debug-console-mcp | awk '{print $5}')"
echo ""
echo "=== Release v$VERSION complete ==="
echo "To push: git push origin v$VERSION"
