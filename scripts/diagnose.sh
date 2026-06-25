#!/bin/bash
# Serial MCP Diagnostic — run when MCP is offline or misbehaving
# Usage: bash scripts/diagnose.sh

DEHOST="${DEHOST:-192.168.1.105}"
RED='\033[31m'; GREEN='\033[32m'; YELLOW='\033[33m'; NC='\033[0m'
ok() { echo -e "  ${GREEN}✓${NC} $1"; }
warn() { echo -e "  ${YELLOW}⚠${NC} $1"; }
fail() { echo -e "  ${RED}✗${NC} $1"; }

echo "=== Serial MCP Diagnostic ==="
echo "Dev Host: $DEHOST"
echo ""

# 1. Binary check
echo "[1] MCP Binary"
if command -v debug-console-mcp &>/dev/null; then
    ok "$(debug-console-mcp --version 2>&1 | head -1)"
else
    fail "debug-console-mcp not in PATH — run: deploy-all.sh"
fi

# 2. Dev host reachable
echo "[2] Dev Host SSH"
if ssh -o ConnectTimeout=3 "$DEHOST" "echo ok" &>/dev/null; then
    ok "$DEHOST reachable"
else
    fail "$DEHOST unreachable"
fi

# 3. ser2net ports
echo "[3] ser2net ports"
for port in 2000 2001 2002 2008; do
    if ssh "$DEHOST" "ss -tlnp | grep -q :$port" 2>/dev/null; then
        ok "port $port LISTEN"
    else
        warn "port $port not listening"
    fi
done

# 4. USB devices
echo "[4] USB serial devices"
ssh "$DEHOST" "ls /dev/serial/by-alias/ 2>/dev/null" | while read alias; do
    ok "$alias → $(ssh "$DEHOST" "readlink /dev/serial/by-alias/$alias" 2>/dev/null)"
done
if ! ssh "$DEHOST" "ls /dev/serial/by-alias/ 2>/dev/null | grep -q ."; then
    warn "no devices in /dev/serial/by-alias/ — check USB cables"
fi

# 5. MCP processes
echo "[5] MCP processes"
MCP_COUNT=$(ps aux | grep -c '[d]ebug-console-mcp')
if [ "$MCP_COUNT" -eq 1 ]; then
    ok "1 MCP running"
elif [ "$MCP_COUNT" -gt 1 ]; then
    warn "$MCP_COUNT MCP processes (may conflict) — run: pkill debug-console-mcp"
else
    warn "no MCP running — Claude Code will auto-start on next tool call"
fi

# 6. Lock files
echo "[6] Lock files"
for lock in /dev/shm/claude-serial-*.lock; do
    if [ -f "$lock" ]; then
        ok "$(basename $lock) → $(cat $lock)"
    fi
done
if ! ls /dev/shm/claude-serial-*.lock &>/dev/null; then
    warn "no lock files — session-start may not have run yet"
fi

# 7. .dut-serial state
echo "[7] Project state"
for dir in /media/loh/rockchip/lr3576_*/.dut-serial; do
    if [ -d "$dir" ]; then
        proj="$(dirname "$dir")"
        for statefile in "$dir"/*/target-state; do
            if [ -f "$statefile" ]; then
                alias="$(basename "$(dirname "$statefile")")"
                state="$(cat "$statefile")"
                ok "$(basename "$proj")/$alias: $state"
            fi
        done
    fi
done

echo ""
echo "=== Diagnostic Complete ==="
