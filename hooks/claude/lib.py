#!/usr/bin/env python3
"""Shared utilities for embedded-debug hooks — fast (<2ms), zero network I/O."""

from __future__ import annotations

import os
import hashlib
from pathlib import Path
from typing import Optional

# Directories that should NEVER be treated as project roots, even if they
# contain a .target.conf file (e.g. leftover from a previous session).
_FORBIDDEN_ROOTS = {"/tmp", "/var/tmp", "/dev/shm", "/run", "/proc", "/sys", "/dev"}

# Companion files/dirs that must exist alongside .target.conf to validate
# it's a real project directory, not a stale file in a temp dir.
_PROJECT_MARKERS = [".dut-serial", ".claude", "build/envsetup.sh", "device/rockchip"]


def project_hash(project_dir: str) -> str:
    """Stable 8-char hash for a project path — used for per-project lock files."""
    return hashlib.md5(str(Path(project_dir).resolve()).encode()).hexdigest()[:8]


def _read_lock(lock_path: str) -> Optional[str]:
    """Read a lock file, return content if valid directory path."""
    p = Path(lock_path)
    if p.exists():
        try:
            content = p.read_text().strip()
            if content and Path(content).is_dir():
                return content
        except OSError:
            pass
    return None


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
    """Find project root — per-hash lock prevents cross-project interference.

    1. TARGET_CONF env var (explicit override)
    2. Walk-up from CWD (current working directory)
    3. Scan /dev/shm/claude-serial-*.lock for valid projects (fallback)
    """
    if env := os.environ.get("TARGET_CONF"):
        p = Path(env)
        if p.exists():
            d = p.resolve().parent
            if _is_valid_project_dir(d):
                return str(d)
    # Walk up from CWD first — the user's current intent
    d = Path.cwd()
    while True:
        for name in (".target.toml", ".target.conf"):
            if (d / name).exists() and _is_valid_project_dir(d):
                return str(d)
        parent = d.parent
        if parent == d:
            break
        d = parent
    # Fallback: scan per-hash locks for any valid project
    import glob
    for lock_path in sorted(glob.glob("/dev/shm/claude-serial-*.lock")):
        cached = _read_lock(lock_path)
        if cached and _is_valid_project_dir(Path(cached)):
            return cached
    return None


def read_dut_configs(project_dir: str) -> dict:
    """Parse .target.toml for [[dut]] entries. Returns {alias: {serial_port, login_user}}.
    Falls back to single-DUT .target.conf shell format if .target.toml missing."""
    import re
    duts = {}
    toml_path = Path(project_dir) / ".target.toml"
    if not toml_path.exists():
        # Fallback: single DUT from shell-format .target.conf
        conf = _read_shell_config(project_dir)
        if conf.get("host"):
            duts["default"] = {"serial_port": conf.get("port", "?"), "login_user": ""}
        return duts
    try:
        with open(toml_path) as f:
            content = f.read()
        # Split on [[dut]] sections (skip content before first [[dut]])
        blocks = re.split(r'\n\s*\[\[dut\]\]', content)
        for block in blocks[1:]:
            alias = re.search(r'alias\s*=\s*"([^"]+)"', block)
            port = re.search(r'port\s*=\s*(\d+)', block)
            login = re.search(r'login_user\s*=\s*"([^"]+)"', block)
            if alias:
                duts[alias.group(1)] = {
                    "serial_port": port.group(1) if port else "?",
                    "login_user": login.group(1) if login else "",
                }
    except Exception:
        pass
    return duts


def _read_shell_config(project_dir: str) -> dict:
    """Read legacy .target.conf shell format. Returns {host, port} dict."""
    conf = Path(project_dir) / ".target.conf"
    result = {}
    if not conf.exists():
        return result
    try:
        for line in conf.read_text().splitlines():
            line = line.strip()
            if line.startswith("DEV_HOST_IP="):
                result["host"] = line.split("=", 1)[1].strip().strip('"').strip("'")
            elif line.startswith("SERIAL_PORT="):
                result["port"] = line.split("=", 1)[1].strip().strip('"').strip("'")
    except OSError:
        pass
    return result


def format_serial_state(state: str, label: str = "serial") -> Optional[tuple[str, str]]:
    """Format state string for statusline. Returns (display_text, color) or None."""
    mapping = {
        "active": (f"● {label}:active", "green"),
        "booting": (f"◐ {label}:booting", "yellow"),
        "uboot": (f"● {label}:uboot", "cyan"),
        "crashed": (f"✗ {label}:crashed", "red"),
        "disconnected": (f"✗ {label}:disconnected", "red"),
        "DUT-off": (f"✗ {label}:DUT-off", "red"),
    }
    return mapping.get(state)


def _is_embedded_server(pid: int) -> bool:
    """Check if a PID belongs to a debug-console-mcp server process."""
    try:
        comm = (Path("/proc") / str(pid) / "comm").read_text().strip()
        if "debug-console" in comm:
            return True
        cmdline = (Path("/proc") / str(pid) / "cmdline").read_text()
        if "debug-console" in cmdline or "server.py" in cmdline:
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
