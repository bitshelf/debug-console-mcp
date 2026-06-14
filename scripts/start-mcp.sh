#!/bin/bash
# MCP + statusline daemon launcher for embedded-debug projects.
# Called by SessionStart hook before Claude Code starts.
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
MCP_BIN="$HOME/.local/bin/embedded-debug-mcp"
WATCH_BIN="$HOME/.local/bin/statusline-watch"
MCP_PORT="${MCP_PORT:-3000}"
MCP_HOST="${MCP_HOST:-0.0.0.0}"
REGISTRY="/dev/shm/.statusline-watch.projects"

PID_FILE="$PROJECT_DIR/.dut-serial/mcp-daemon.pid"
LOG_FILE="$PROJECT_DIR/.dut-serial/mcp-http.log"
STATE_FILE="$PROJECT_DIR/.dut-serial/target-state"
LOCK_FILE="$PROJECT_DIR/.dut-serial/mcp.lock"

# ── flock singleton ─────────────────────────────────────────────────────
exec {LOCK_FD}>"$LOCK_FILE"
if ! flock -n "$LOCK_FD"; then
    if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
        exit 0
    fi
    flock "$LOCK_FD" || exit 1
fi

# ── Port conflict ───────────────────────────────────────────────────────
free_port() {
    local port="$1" pid
    pid=$(ss -tlnpH "sport = :$port" 2>/dev/null | grep -oP 'pid=\K\d+' | head -1) || true
    if [ -n "${pid:-}" ]; then
        if cat "/proc/$pid/cmdline" 2>/dev/null | grep -q "embedded-debug-mcp"; then
            echo "Killing old MCP on port $port (PID $pid)..."
            kill "$pid" 2>/dev/null || true
            for _ in $(seq 1 20); do
                ss -tlnpH "sport = :$port" 2>/dev/null | grep -q "pid=$pid" || break
                sleep 0.1
            done
        else
            echo "ERROR: Port $port in use by PID $pid (non-MCP), set MCP_PORT"
            exit 1
        fi
    fi
}

# ── Main ────────────────────────────────────────────────────────────────
mkdir -p "$PROJECT_DIR/.dut-serial"
free_port "$MCP_PORT"
cd "$PROJECT_DIR"
echo "connecting" > "$STATE_FILE"

nohup "$MCP_BIN" --http "$MCP_HOST:$MCP_PORT" > "$LOG_FILE" 2>&1 &
MCP_PID=$!
echo "$MCP_PID" > "$PID_FILE"
echo "MCP started (PID $MCP_PID, port $MCP_PORT)"

sleep 1
if ! kill -0 "$MCP_PID" 2>/dev/null; then
    echo "ERROR: MCP failed to start (check $LOG_FILE)"
    echo "disconnected" > "$STATE_FILE"
    exit 1
fi

# ── Post-start serial probe ─────────────────────────────────────────────
(
    sleep 3
    TARGET_CONF="$PROJECT_DIR/.target.conf"
    if [ -f "$TARGET_CONF" ]; then
        HOST=$(grep -oP '^DEV_HOST_IP=\K.*' "$TARGET_CONF" | tr -d '"' | tr -d "'")
        PORT=$(grep -oP '^SERIAL_PORT=\K.*' "$TARGET_CONF" | tr -d '"' | tr -d "'")
        if [ -n "${HOST:-}" ] && [ -n "${PORT:-}" ]; then
            printf '\n' | timeout 3 bash -c "exec 3<>/dev/tcp/$HOST/$PORT 2>/dev/null && printf '\n' >&3 && exec 3>&-" 2>/dev/null || true
        fi
    fi
) &

# ── Ensure statusline-watch daemon ──────────────────────────────────────
echo "$PROJECT_DIR" >> "$REGISTRY"
sort -u "$REGISTRY" -o "$REGISTRY"
if [ -x "$WATCH_BIN" ] && ! pgrep -f "statusline-watch" > /dev/null 2>&1; then
    nohup "$WATCH_BIN" --registry "$REGISTRY" > /tmp/statusline-watch.log 2>&1 &
    disown
fi
