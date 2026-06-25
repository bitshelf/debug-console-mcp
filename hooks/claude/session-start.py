#!/usr/bin/env python3
"""SessionStart hook — discover the current project and start the MCP HTTP server.

Projects only need `.mcp.json` + `.target.conf`. No per-project scripts.

In **stdio** mode (the default, `.mcp.json` has `command` not `url`), Claude Code
itself spawns the MCP server from `.mcp.json` — this hook does NOT start it.
In **HTTP** mode (`.mcp.json` has `url`), this hook starts the HTTP server.
"""

import json
import os
import subprocess
import sys
import time
from pathlib import Path
from subprocess import Popen, DEVNULL

# Reuse the shared project-discovery logic so behavior is consistent with the
# other hooks (walks up from CWD, validates project markers, respects TARGET_CONF).
from lib import find_project_dir, _is_embedded_server


def find_projects():
    """Discover the project Claude Code is currently working in.

    Walks up from CWD to find a validated `.target.conf` (same rule as the Rust
    config loader and the other hooks), so launching Claude Code from a
    subdirectory still works.
    """
    proj = find_project_dir()
    return [proj] if proj else []


def read_mcp_port(project_dir):
    """Read HTTP port from the project's `.mcp.json`. Returns int or None.

    Only HTTP-mode configs have a `url` field. stdio-mode configs return None,
    which means the MCP is spawned by Claude Code itself and this hook does not
    start a server.
    """
    mcp_json = Path(project_dir) / ".mcp.json"
    if not mcp_json.exists():
        return None
    try:
        cfg = json.loads(mcp_json.read_text())
        url = cfg.get("mcpServers", {}).get("debug-console", {}).get("url", "")
        # "http://localhost:3000/mcp" → 3000
        if ":" in url:
            return int(url.rsplit(":", 1)[-1].split("/")[0])
    except (json.JSONDecodeError, ValueError, KeyError):
        pass
    return None


def is_port_in_use(port):
    import socket
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        s.connect(("127.0.0.1", port))
        s.close()
        return True
    except (ConnectionRefusedError, OSError):
        return False


def mcp_running(project_dir):
    """Check if a debug-console-mcp process is already serving this project."""
    try:
        for entry in Path("/proc").iterdir():
            if not entry.name.isdigit():
                continue
            try:
                pid = int(entry.name)
                if pid == os.getpid():
                    continue
                if not _is_embedded_server(pid):
                    continue
                cwd = os.readlink(f"{entry}/cwd")
                if Path(cwd).resolve() == Path(project_dir).resolve():
                    return True
            except (OSError, FileNotFoundError, ValueError):
                continue
    except OSError:
        pass
    return False


def _kill_stale_mcp_on_port(port):
    """Kill a debug-console-mcp process listening on the given port.

    Validates the PID is actually a debug-console-mcp process (via /proc/comm)
    before killing — never kills an unrelated process that happens to hold the
    port.
    """
    result = subprocess.run(
        ["ss", "-tlnpH", f"sport = :{port}"],
        capture_output=True, text=True
    )
    for line in result.stdout.splitlines():
        if "pid=" not in line:
            continue
        old_pid = line.split("pid=")[-1].split(",")[0]
        try:
            pid = int(old_pid)
        except ValueError:
            continue
        # Validate before killing.
        if not _is_embedded_server(pid):
            continue
        try:
            os.kill(pid, 15)
        except OSError:
            pass
    time.sleep(0.5)


def start_mcp(project_dir, port):
    """Start debug-console-mcp in HTTP mode for a project."""
    if mcp_running(project_dir):
        return
    if is_port_in_use(port):
        _kill_stale_mcp_on_port(port)
        if is_port_in_use(port):
            return

    binary = os.path.expanduser("~/.local/bin/debug-console-mcp")
    if not os.path.isfile(binary):
        return

    Popen(
        [binary, "--http", f"127.0.0.1:{port}"],
        cwd=project_dir,
        stdout=DEVNULL, stderr=DEVNULL,
        start_new_session=True,
    )


def main():
    for proj in find_projects():
        dut_dir = os.path.join(proj, ".dut-serial")
        os.makedirs(dut_dir, exist_ok=True)

        port = read_mcp_port(proj)
        if port is None:
            # stdio mode — Claude Code spawns the MCP from .mcp.json itself.
            continue

        start_mcp(proj, port)

    sys.exit(0)


if __name__ == "__main__":
    main()
