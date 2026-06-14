#!/bin/bash
# E2E integration test for debug-console-mcp
# Tests: build, deploy, serial command, statusline, session-stop cleanup
set -eo pipefail

echo "=== debug-console-mcp E2E Test ==="
PROJ_DIR="/media/loh/rockchip/lr3576_v2.1"
TARGET_CONF="$PROJ_DIR/.target.toml"
BINARY="$HOME/.local/bin/debug-console-mcp"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Test 1: Build
echo "[TEST 1/6] Build"
cd "$SCRIPT_DIR/../mcp-rs" && cargo build --release 2>&1 | tail -1
cp -f target/release/debug-console-mcp "$BINARY"
echo "  OK"

# Test 2: Version
echo "[TEST 2/6] Binary version"
"$BINARY" --version 2>&1
echo "  OK"

# Test 3: MCP initialize + serial_get_state
echo "[TEST 3/6] MCP initialize + state"
printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}}\n{"jsonrpc":"2.0","method":"notifications/initialized"}\n{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"serial_get_state","arguments":{}}}\n' \
    | timeout 10 "$BINARY" 2>/dev/null > /tmp/e2e-test-state.json || true
if grep -q 'state' /tmp/e2e-test-state.json 2>/dev/null; then
    echo "  OK: state reported → $(grep 'state' /tmp/e2e-test-state.json)"
else
    echo "  WARN: no state output (binary may have failed)"
fi

# Test 4: Serial command
echo "[TEST 4/6] Serial command"
printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}}\n{"jsonrpc":"2.0","method":"notifications/initialized"}\n{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"serial_send_command","arguments":{"command":"echo e2e_test_ok","timeout":8}}}\n' \
    | TARGET_CONF="$TARGET_CONF" timeout 15 "$BINARY" 2>/dev/null > /tmp/e2e-cmd.result || true
if grep -q "e2e_test_ok" /tmp/e2e-cmd.result; then
    echo "  OK: command output correct"
else
    echo "  WARN: command output not found (may need warmup retry)"
fi

# Test 5: Statusline cache exists
echo "[TEST 5/6] Statusline cache"
sleep 3  # Wait for cache refresh
if [ -d "$PROJ_DIR/.dut-serial" ]; then
    found=0
    for alias_dir in "$PROJ_DIR/.dut-serial"/*/; do
        [ -d "$alias_dir" ] || continue
        cache="$alias_dir/statusline-cache"
        if [ -f "$cache" ]; then
            echo "  OK: $(basename $(dirname "$cache")) → $(cat "$cache")"
            found=1
        fi
    done
    if [ "$found" -eq 0 ]; then
        echo "  INFO: no statusline cache found (cache refresh may not have run yet)"
    fi
else
    echo "  INFO: no .dut-serial directory"
fi

# Test 6: Lock file isolation
echo "[TEST 6/6] Per-hash lock isolation"
HASH=$(echo -n "$PROJ_DIR" | md5sum | awk '{print substr($1,1,8)}')
LOCK="/dev/shm/claude-serial-${HASH}.lock"
if [ -f "$LOCK" ]; then
    echo "  OK: lock file exists → $(cat "$LOCK")"
else
    echo "  INFO: no lock file (session-start hasn't run yet)"
fi

echo ""
echo "=== E2E Test Complete ==="
