#!/usr/bin/env python3
"""SessionStart hook — discover projects, start MCP + statusline-watch daemon.

Projects only need .mcp.json + .target.conf. No per-project scripts.
"""

import json
import os
import subprocess
import sys
import time
from pathlib import Path
from subprocess import Popen, DEVNULL


def find_projects():
    """Only discover the project Claude Code is currently working in.

    Checks CWD directly for .target.conf — no directory scanning at all.
    """
    cwd = os.getcwd()
    if os.path.isfile(os.path.join(cwd, ".target.conf")):
        return [cwd]
    return []


def project_hash(project_dir):
    import hashlib
    return hashlib.md5(str(Path(project_dir).resolve()).encode()).hexdigest()[:8]


def read_mcp_port(project_dir):
    """Read HTTP port from project's .mcp.json serial config. Returns int or None."""
    mcp_json = Path(project_dir) / ".mcp.json"
    if not mcp_json.exists():
        return None
    try:
        cfg = json.loads(mcp_json.read_text())
        url = cfg.get("mcpServers", {}).get("embedded-debug", {}).get("url", "")
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
    """Check if an embedded-debug-mcp process is already serving this project."""
    try:
        for entry in Path("/proc").iterdir():
            if not entry.name.isdigit():
                continue
            try:
                pid = int(entry.name)
                if pid == os.getpid():
                    continue
                comm = (entry / "comm").read_text().strip()
                if "embedded" not in comm:
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
    """Kill any embedded-debug-mcp process listening on the given port."""
    result = subprocess.run(
        ["ss", "-tlnpH", f"sport = :{port}"],
        capture_output=True, text=True
    )
    for line in result.stdout.splitlines():
        if "pid=" in line:
            old_pid = line.split("pid=")[-1].split(",")[0]
            try:
                os.kill(int(old_pid), 15)
            except OSError:
                pass
    time.sleep(0.5)


def start_mcp(project_dir, port):
    """Start embedded-debug-mcp in HTTP mode for a project."""
    if mcp_running(project_dir):
        return
    if is_port_in_use(port):
        _kill_stale_mcp_on_port(port)
        if is_port_in_use(port):
            return

    binary = os.path.expanduser("~/.local/bin/embedded-debug-mcp")
    if not os.path.isfile(binary):
        return

    Popen(
        [binary, "--http", f"0.0.0.0:{port}"],
        cwd=project_dir,
        stdout=DEVNULL, stderr=DEVNULL,
        start_new_session=True,
    )


def start_daemon(registry):
    """Start statusline-watch daemon if not running."""
    result = subprocess.run(["pgrep", "-cf", "statusline-watch"], capture_output=True, text=True)
    if int(result.stdout.strip() or 0) > 0:
        return
    binary = os.path.expanduser("~/.local/bin/statusline-watch")
    if not os.path.isfile(binary):
        return
    Popen(
        [binary, "--registry", registry],
        stdout=DEVNULL, stderr=DEVNULL,
        start_new_session=True,
    )


def main():
    registry = "/dev/shm/.statusline-watch.projects"
    seen = set()

    for proj in find_projects():
        dut_dir = os.path.join(proj, ".dut-serial")
        os.makedirs(dut_dir, exist_ok=True)

        port = read_mcp_port(proj)
        if port is None:
            continue

        start_mcp(proj, port)
        if proj not in seen:
            seen.add(proj)

    # Write registry for daemon
    with open(registry, "w") as f:
        for proj in sorted(seen):
            f.write(proj + "\n")

    start_daemon(registry)
    sys.exit(0)


if __name__ == "__main__":
    main()
