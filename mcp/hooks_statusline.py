#!/usr/bin/env python3
"""
Statusline hook — git branch + target serial state.

CRITICAL: Always returns in <50ms. NO network I/O, NO subprocess calls.
State is read from tiny files (cached). Git branch uses a file-based cache
with async background refresh.

Output format:  [model] gitbranch  ● serial:state
"""

import os
import sys
import json
import time
from pathlib import Path
from subprocess import Popen, DEVNULL

# Ensure hooks directory is on sys.path for import
_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

try:
    from lib import find_project_dir, get_session_id, is_daemon_alive, read_state, format_serial_state
except ImportError:
    from hooks_lib import find_project_dir, get_session_id, is_daemon_alive, read_state, format_serial_state


# ── Config ──────────────────────────────────────────────────────────────────

CACHE_TTL = 10          # seconds before git cache is considered stale
TMP_ROOT = os.environ.get("TMPDIR", os.environ.get("TMP", "/tmp"))
# Prefer /dev/shm for in-memory speed
if os.path.isdir("/dev/shm") and os.access("/dev/shm", os.W_OK):
    CACHE_DIR = "/dev/shm"
else:
    CACHE_DIR = TMP_ROOT


# ── Git branch cache ────────────────────────────────────────────────────────

def _git_cache_path() -> str:
    """Cache file path scoped to current project directory (by hash)."""
    import hashlib
    h = hashlib.md5(os.getcwd().encode()).hexdigest()[:8]
    return f"{CACHE_DIR}/claude-git-{h}.cache"


def _read_git_cache():  # -> Optional[str]
    """Read cached git branch. Returns branch name or None."""
    cache = _git_cache_path()
    try:
        with open(cache, "r") as f:
            ts_str, branch = f.readline().strip().split(" ", 1)
            ts = float(ts_str)
            if time.time() - ts < CACHE_TTL:
                return branch
    except (FileNotFoundError, ValueError, OSError):
        pass
    return None


def _refresh_git_cache_async() -> None:
    """Trigger async refresh of git branch cache. Non-blocking."""
    cache = _git_cache_path()
    lock_dir = cache + ".lock"

    # Check if refresh is already in progress
    if os.path.isdir(lock_dir):
        # Check if lock is stale (> 15s)
        try:
            mtime = os.path.getmtime(lock_dir)
            if time.time() - mtime < 15:
                return
            os.rmdir(lock_dir)  # Stale lock — remove
        except OSError:
            return

    # Try to acquire lock
    try:
        os.mkdir(lock_dir)
    except FileExistsError:
        return

    # Background refresh
    try:
        Popen(
            [
                "bash", "-c",
                f'cd "{os.getcwd()}" && '
                f'BRANCH=$(git branch --show-current 2>/dev/null || git rev-parse --short HEAD 2>/dev/null || echo "?"); '
                f'git diff --quiet 2>/dev/null && git diff --cached --quiet 2>/dev/null || BRANCH="$BRANCH*"; '
                f'echo "$(date +%s) $BRANCH" > "{cache}" 2>/dev/null; '
                f'rmdir "{lock_dir}" 2>/dev/null'
            ],
            stdout=DEVNULL, stderr=DEVNULL,
            start_new_session=True,
        )
    except OSError:
        try:
            os.rmdir(lock_dir)
        except OSError:
            pass


def _is_git_repo() -> bool:
    """Check if CWD is inside a git repo. Fast: checks .git file/dir only."""
    git = Path(".git")
    if git.exists():
        return True  # .git dir or .git file (worktree)
    return False


def _compute_git_branch() -> str:
    """Compute git branch inline (one-time cost on cache miss, ~10ms)."""
    import subprocess
    try:
        r = subprocess.run(
            ["git", "branch", "--show-current"],
            capture_output=True, text=True, timeout=1
        )
        branch = r.stdout.strip()
        if not branch:
            r = subprocess.run(
                ["git", "rev-parse", "--short", "HEAD"],
                capture_output=True, text=True, timeout=1
            )
            branch = r.stdout.strip() or "?"
        # Include dirty marker
        r2 = subprocess.run(
            ["git", "diff", "--quiet"],
            capture_output=True, timeout=1
        )
        if r2.returncode != 0:
            branch += "*"
        else:
            r3 = subprocess.run(
                ["git", "diff", "--cached", "--quiet"],
                capture_output=True, timeout=1
            )
            if r3.returncode != 0:
                branch += "*"
        return branch
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError):
        return "?"


def _write_git_cache(branch: str) -> None:
    """Write git branch to cache file."""
    cache = _git_cache_path()
    try:
        with open(cache, "w") as f:
            f.write(f"{time.time()} {branch}")
    except OSError:
        pass


def _get_git_branch() -> str:
    """Get git branch for statusline. Cache-first, <5ms on cache hit."""
    if not _is_git_repo():
        return os.path.basename(os.getcwd())

    # Cache hit — fastest path (~1ms file read)
    cached = _read_git_cache()
    if cached is not None:
        # Trigger async refresh if close to TTL
        return cached

    # Cache miss (first call for this project) — compute once
    _refresh_git_cache_async()
    branch = _compute_git_branch()
    if branch and branch != "?":
        _write_git_cache(branch)
    return branch if branch else os.path.basename(os.getcwd())


# ── Main ────────────────────────────────────────────────────────────────────

def main():
    # Parse stdin for model info (Claude Code passes JSON)
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

    git_branch = _get_git_branch()
    left_parts.append(git_branch)
    left = " ".join(left_parts)

    # Right side: target serial state
    right = ""
    project_dir = find_project_dir()
    if project_dir:
        dut = Path(project_dir) / ".dut-serial"
        state_file = dut / "target-state"
        state = None

        if state_file.exists():
            try:
                state = state_file.read_text().strip()
            except OSError:
                pass

        if state:
            formatted = format_serial_state(state)
            if formatted:
                display_text, color = formatted
                ansi = {
                    "green": "\033[32m", "red": "\033[31m",
                    "cyan": "\033[36m", "yellow": "\033[33m",
                }
                code = ansi.get(color, "")
                reset = "\033[0m" if code else ""
                right = f"{code}{display_text}{reset}"

    # Output — one line, fast
    if right:
        print(f"{left}  {right}")
    else:
        print(left)


if __name__ == "__main__":
    main()
