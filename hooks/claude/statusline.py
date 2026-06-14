#!/usr/bin/env python3
"""
Statusline hook — reads MCP-written cache file, formats git branch + model.

Architecture (event-driven, zero polling):
  MCP Server ──atomic_write──→ .dut-serial/statusline-cache
  Statusline hook reads cache file (<1ms), no daemon, no inotify, no network I/O.
"""

import os
import sys
import json
import time
from pathlib import Path
from subprocess import Popen, DEVNULL

_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

from lib import find_project_dir, format_serial_state, _is_valid_project_dir

# ── Config ──────────────────────────────────────────────────────────────────
TMP_ROOT = os.environ.get("TMPDIR", os.environ.get("TMP", "/tmp"))
CACHE_DIR = "/dev/shm" if os.path.isdir("/dev/shm") and os.access("/dev/shm", os.W_OK) else TMP_ROOT
CACHE_TTL = 10


# ── Git branch (cached, async refresh) ─────────────────────────────────────

def _project_root() -> str:
    d = Path.cwd()
    while True:
        if (d / ".target.conf").exists() and _is_valid_project_dir(d):
            return str(d)
        parent = d.parent
        if parent == d:
            break
        d = parent
    return str(Path.cwd())


def _cache_key(suffix: str) -> str:
    import hashlib
    h = hashlib.md5(_project_root().encode()).hexdigest()[:8]
    return f"{CACHE_DIR}/claude-{h}-{suffix}.cache"


def _git_cache_path() -> str:
    return _cache_key("git")


def _read_git_cache():
    cache = _git_cache_path()
    try:
        with open(cache, "r") as f:
            ts_str, branch = f.readline().strip().split(" ", 1)
            if time.time() - float(ts_str) < CACHE_TTL:
                return branch
    except (FileNotFoundError, ValueError, OSError):
        pass
    return None


def _refresh_git_cache_async() -> None:
    cache = _git_cache_path()
    lock_dir = cache + ".lock"
    if os.path.isdir(lock_dir):
        try:
            if time.time() - os.path.getmtime(lock_dir) < 15:
                return
            os.rmdir(lock_dir)
        except OSError:
            return
    try:
        os.mkdir(lock_dir)
    except FileExistsError:
        return
    try:
        Popen(
            ["bash", "-c",
             f'cd "{os.getcwd()}" && '
             f'BRANCH=$(git branch --show-current 2>/dev/null || git rev-parse --short HEAD 2>/dev/null || echo "?"); '
             f'git diff --quiet 2>/dev/null && git diff --cached --quiet 2>/dev/null || BRANCH="$BRANCH*"; '
             f'echo "$(date +%s) $BRANCH" > "{cache}" 2>/dev/null; '
             f'rmdir "{lock_dir}" 2>/dev/null'],
            stdout=DEVNULL, stderr=DEVNULL, start_new_session=True)
    except OSError:
        try:
            os.rmdir(lock_dir)
        except OSError:
            pass


def _is_git_repo() -> bool:
    return Path(".git").exists()


def _compute_git_branch() -> str:
    import subprocess
    try:
        r = subprocess.run(
            ["git", "branch", "--show-current"],
            capture_output=True, text=True, timeout=1)
        branch = r.stdout.strip()
        if not branch:
            r = subprocess.run(
                ["git", "rev-parse", "--short", "HEAD"],
                capture_output=True, text=True, timeout=1)
            branch = r.stdout.strip() or "?"
        for flag in [["git", "diff", "--quiet"], ["git", "diff", "--cached", "--quiet"]]:
            r2 = subprocess.run(flag, capture_output=True, timeout=1)
            if r2.returncode != 0:
                branch += "*"
                break
        return branch
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError):
        return "?"


def _write_git_cache(branch: str) -> None:
    cache = _git_cache_path()
    try:
        with open(cache, "w") as f:
            f.write(f"{time.time()} {branch}")
    except OSError:
        pass


def _get_git_branch() -> str:
    if not _is_git_repo():
        return os.path.basename(os.getcwd())
    cached = _read_git_cache()
    if cached is not None:
        return cached
    _refresh_git_cache_async()
    branch = _compute_git_branch()
    if branch and branch != "?":
        _write_git_cache(branch)
    return branch if branch else os.path.basename(os.getcwd())


# ── Serial state (read from MCP-written cache) ──────────────────────────────

def _read_serial_text(project_dir: str) -> str:
    """Read ANSI-formatted serial state from MCP's cache file.

    The MCP writes .dut-serial/statusline-cache on every state transition.
    Falls back to reading target-state + formatting if cache is missing.
    """
    # Primary: read MCP's pre-formatted cache
    cache = Path(project_dir) / ".dut-serial" / "statusline-cache"
    if cache.exists():
        try:
            text = cache.read_text().strip()
            if text:
                return text
        except OSError:
            pass

    # Fallback: read target-state and format locally
    state_file = Path(project_dir) / ".dut-serial" / "target-state"
    if state_file.exists():
        try:
            state = state_file.read_text().strip()
            if state:
                formatted = format_serial_state(state)
                if formatted:
                    display_text, color = formatted
                    ansi = {"green": "\033[32m", "red": "\033[31m",
                            "cyan": "\033[36m", "yellow": "\033[33m"}
                    code = ansi.get(color, "")
                    reset = "\033[0m" if code else ""
                    return f"{code}{display_text}{reset}"
        except OSError:
            pass

    return ""


# ── Main ────────────────────────────────────────────────────────────────────

def main():
    # Parse model from stdin (Claude Code passes JSON)
    model = ""
    try:
        stdin_data = sys.stdin.read()
        if stdin_data.strip():
            obj = json.loads(stdin_data)
            model = obj.get("model", {}).get("display_name", "")
    except (json.JSONDecodeError, OSError):
        pass

    # Left side: model + git branch
    left_parts = []
    if model:
        left_parts.append(f"[{model}]")
    left_parts.append(_get_git_branch())
    left = " ".join(left_parts)

    # Right side: serial state (from MCP cache)
    project_dir = find_project_dir()
    serial_text = _read_serial_text(project_dir) if project_dir else ""

    if serial_text:
        print(f"{left}  {serial_text}")
    else:
        print(left)


if __name__ == "__main__":
    main()
