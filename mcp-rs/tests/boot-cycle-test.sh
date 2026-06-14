#!/bin/bash
# Boot cycle reliability test — verifies serial tools respond correctly
set -e
BINARY="$HOME/.local/bin/debug-console-mcp"
TARGET_CONF="${TARGET_CONF:-/media/loh/rockchip/lr3576_v2.1/.target.toml}"
echo "=== Boot Cycle Reliability Test ==="

mcp_call() {
    printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}\n{"jsonrpc":"2.0","method":"notifications/initialized"}\n%s\n' "$1" | TARGET_CONF="$TARGET_CONF" timeout 10 "$BINARY" 2>/dev/null
}

echo "[1/4] serial_get_state"
mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"serial_get_state","arguments":{}}}' | grep -q '"state"' && echo "  OK" || echo "  FAIL"

echo "[2/4] serial_get_logs"
mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"serial_get_logs","arguments":{"lines":10}}}' | grep -q '"content"' && echo "  OK" || echo "  FAIL"

echo "[3/4] serial_list_logs"
mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"serial_list_logs","arguments":{}}}' | grep -q '"archives"' && echo "  OK" || echo "  FAIL"

echo "[4/4] serial_get_config"
mcp_call '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"serial_get_config","arguments":{}}}' | grep -q '"relay_configured"' && echo "  OK" || echo "  FAIL"

echo "=== Boot Cycle Test Complete ==="
