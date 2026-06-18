#!/usr/bin/env python3
"""Shared utilities for embedded-debug hooks — fast (<2ms), zero network I/O."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Optional

# Directories that should NEVER be treated as project roots, even if they
# contain a .target.conf file (e.g. leftover from a previous session).
_FORBIDDEN_ROOTS = {"/tmp", "/var/tmp", "/dev/shm", "/run", "/proc", "/sys", "/dev"}

# Companion files/dirs that must exist alongside .target.conf to validate
# it's a real project directory, not a stale file in a temp dir.
_PROJECT_MARKERS = [".dut-serial", ".claude", "build/envsetup.sh", "device/rockchip"]


def _is_valid_project_dir(d: Path) -> bool:
    """Reject only the forbidden roots themselves (e.g. /tmp/.target.conf),
    not subdirectories (e.g. /tmp/project/.target.conf with markers is fine)."""
    resolved = str(d.resolve())
    for forbidden in _FORBIDDEN_ROOTS:
        if resolved == forbidden:
            return False
    for marker in _PROJECT_MARKERS:
        if (d / marker).exists():
            return True
    return False


def find_project_dir() -> Optional[str]:
    """Find project root by walking up from CWD to find .target.toml or
    .target.conf (TOML preferred).

    Only returns directories that pass _is_valid_project_dir() — a stray
    config in /tmp (from a previous session) will be ignored.
    """
    if env := os.environ.get("TARGET_CONF"):
        p = Path(env)
        if p.exists():
            d = p.resolve().parent
            if _is_valid_project_dir(d):
                return str(d)
    d = Path.cwd()
    while True:
        # Prefer TOML, fall back to shell conf (matches Rust config.rs).
        for name in (".target.toml", ".target.conf"):
            if (d / name).exists() and _is_valid_project_dir(d):
                return str(d)
        parent = d.parent
        if parent == d:
            break
        d = parent
    return None


def format_serial_state(state: str) -> Optional[tuple[str, str]]:
    """Format state string for statusline. Returns (display_text, color) or None."""
    mapping = {
        "active": ("● serial:active", "green"),
        "booting": ("◐ serial:booting", "yellow"),
        "uboot": ("● serial:uboot", "cyan"),
        "crashed": ("✗ serial:crashed", "red"),
        "disconnected": ("✗ serial:disconnected", "red"),
        "DUT-off": ("✗ serial:DUT-off", "red"),
    }
    return mapping.get(state)


def _is_embedded_server(pid: int) -> bool:
    """Check if a PID belongs to an embedded-debug-mcp server process."""
    try:
        comm = (Path("/proc") / str(pid) / "comm").read_text().strip()
        if "embedded" in comm:
            return True
        cmdline = (Path("/proc") / str(pid) / "cmdline").read_text()
        if "embedded-debug" in cmdline or "server.py" in cmdline:
            return True
    except (FileNotFoundError, OSError):
        pass
    return False


def _mcp_serves_project(pid: int, project_dir: str) -> bool:
    """Check if an MCP process CWD matches the given project directory."""
    try:
        cwd = os.readlink(f"/proc/{pid}/cwd")
        return Path(cwd).resolve() == Path(project_dir).resolve()
    except (OSError, FileNotFoundError):
        return False


def _repair_stale_pid_file(project_dir: str, pid: int) -> None:
    """Write the correct PID to project mcp.pid files (auto-repair stale entries)."""
    for subdir in [".dut-serial", "mcp-rs/.dut-serial"]:
        pid_dir = Path(project_dir) / subdir
        if pid_dir.is_dir():
            try:
                (pid_dir / "mcp.pid").write_text(str(pid))
            except OSError:
                pass


def check_mcp_alive(project_dir: str) -> bool:
    """Check if MCP server process is alive AND serving this project.

    Strategy (in order):
    1. PID file check — fast path.
    2. /proc scan fallback — finds the MCP even if PID file is stale.
    3. Auto-repair: if a live MCP is found via /proc, fix the stale PID file.

    Used by user-prompt-submit.py to verify MCP is running before sending commands.
    """
    for subdir in [".dut-serial", "mcp-rs/.dut-serial"]:
        pid_file = Path(project_dir) / subdir / "mcp.pid"
        if not pid_file.exists():
            continue
        try:
            pid = int(pid_file.read_text().strip())
            os.kill(pid, 0)
            if _is_embedded_server(pid) and _mcp_serves_project(pid, project_dir):
                return True
        except (ValueError, OSError, ProcessLookupError, FileNotFoundError):
            continue

    # /proc fallback
    try:
        for entry in Path("/proc").iterdir():
            if not entry.name.isdigit():
                continue
            try:
                pid = int(entry.name)
                if _is_embedded_server(pid) and _mcp_serves_project(pid, project_dir):
                    _repair_stale_pid_file(project_dir, pid)
                    return True
            except (ValueError, OSError):
                continue
    except OSError:
        pass

    return False
