#!/usr/bin/env python3
"""SessionStart hook — auto-start MCP HTTP server + statusline when .target.conf exists.

Only checks Claude Code CWD (no parent directory walk).
"""

import os
import sys
from pathlib import Path
from subprocess import Popen, DEVNULL

from lib import has_target_conf


def _start_statusline_daemon() -> None:
    """Start the event-driven statusline daemon (inotify watcher)."""
    try:
        Popen(
            [sys.executable,
             os.path.expanduser("~/.claude/hooks/embedded-debug/statusline.py"),
             "--daemon"],
            stdout=DEVNULL, stderr=DEVNULL,
            start_new_session=True,
        )
    except OSError:
        pass


def _ensure_mcp_json(project_dir: str) -> None:
    """Auto-generate .mcp.json with embedded-debug server if .target.conf exists."""
    import json
    mcp_json = Path(project_dir) / ".mcp.json"
    if mcp_json.exists():
        return

    # Check if we have embedded-debug binary installed
    import shutil
    binary = shutil.which("embedded-debug-mcp")
    if not binary:
        binary = os.path.expanduser("~/.local/bin/embedded-debug-mcp")
        if not Path(binary).exists():
            return

    try:
        cfg = {
            "mcpServers": {
                "embedded-debug": {
                    "type": "stdio",
                    "command": binary,
                    "timeout": 60000
                }
            }
        }
        mcp_json.write_text(json.dumps(cfg, indent=2) + "\n")
    except OSError:
        pass


def _find_mcp_binary() -> str | None:
    """Find the embedded-debug-mcp binary."""
    import shutil
    binary = shutil.which("embedded-debug-mcp")
    if binary:
        return binary
    path = os.path.expanduser("~/.local/bin/embedded-debug-mcp")
    if Path(path).exists():
        return path
    return None


def _mcp_already_running() -> bool:
    """Check if MCP HTTP server is already running on port 3000."""
    import socket
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        s.connect(("127.0.0.1", 3000))
        s.close()
        return True
    except (ConnectionRefusedError, OSError):
        return False


def main():
    # 只检测 Claude Code 当前目录的 .target.conf
    if not has_target_conf():
        sys.exit(0)

    project_dir = str(Path.cwd())

    # 1. 启动 statusline daemon
    _start_statusline_daemon()

    # 2. 生成 .mcp.json
    _ensure_mcp_json(project_dir)

    # 3. MCP 由 Claude Code 通过 .mcp.json (stdio transport) 自动启动

    sys.exit(0)


if __name__ == "__main__":
    main()
