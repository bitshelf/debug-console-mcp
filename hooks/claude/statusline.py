#!/usr/bin/env python3
"""
Statusline hook — event-driven via inotify + cache.

Architecture:
  MCP Server ──atomic_write──→ target-state ──inotify──→ daemon ──→ cache file
  Statusline hook reads cache file (<1ms), no polling, no network I/O.

If called with --daemon: runs as background inotify watcher.
Otherwise: fast cache reader for Claude Code statusline hook.
"""

import os
import sys
import json
import time
import ctypes
import ctypes.util
import select
import struct
import errno
import signal
from pathlib import Path
from subprocess import Popen, DEVNULL

_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

from lib import find_project_dir, format_serial_state, check_mcp_alive

# ── Config ──────────────────────────────────────────────────────────────────
TMP_ROOT = os.environ.get("TMPDIR", os.environ.get("TMP", "/tmp"))
CACHE_DIR = "/dev/shm" if os.path.isdir("/dev/shm") and os.access("/dev/shm", os.W_OK) else TMP_ROOT
CACHE_TTL = 10


# ── Git branch cache ────────────────────────────────────────────────────────

def _project_root() -> str:
    """Walk up from CWD to find .target.conf, return that dir. Fallback to CWD."""
    d = Path.cwd()
    while True:
        if (d / ".target.conf").exists():
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


# ── Inotify daemon ───────────────────────────────────────────────────────────

# Linux inotify constants
IN_MODIFY = 0x00000002
IN_CLOSE_WRITE = 0x00000008
IN_MOVED_TO = 0x00000080

_libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)

def _inotify_init():
    fd = _libc.inotify_init1(0)
    if fd == -1:
        raise OSError(ctypes.get_errno(), "inotify_init1 failed")
    return fd

def _inotify_add_watch(fd, path, mask):
    wd = _libc.inotify_add_watch(fd, path.encode(), ctypes.c_uint(mask))
    if wd == -1:
        raise OSError(ctypes.get_errno(), f"inotify_add_watch failed for {path}")
    return wd

def _read_state_file(project_dir):
    """Read current target state from either daemon location."""
    for subdir in [".dut-serial", "mcp-rs/.dut-serial"]:
        sf = Path(project_dir) / subdir / "target-state"
        if sf.exists():
            try:
                state = sf.read_text().strip()
                if state:
                    if not check_mcp_alive(project_dir):
                        return "disconnected"
                    return state
            except OSError:
                pass
    return None

def _compute_serial_text(project_dir):
    """Return only the serial state portion (formatted with ANSI)."""
    state = _read_state_file(project_dir)
    if state:
        formatted = format_serial_state(state)
        if formatted:
            display_text, color = formatted
            ansi = {"green": "\033[32m", "red": "\033[31m", "cyan": "\033[36m", "yellow": "\033[33m"}
            code = ansi.get(color, "")
            reset = "\033[0m" if code else ""
            return f"{code}{display_text}{reset}"
    return ""

def _compute_full_statusline(project_dir, model):
    """Compute full statusline: [model] gitbranch  serial:state."""
    left_parts = []
    if model:
        left_parts.append(f"[{model}]")
    left_parts.append(_get_git_branch())
    left = " ".join(left_parts)
    right = _compute_serial_text(project_dir)
    if right:
        return f"{left}  {right}"
    return left

def run_daemon():
    """Background daemon: watch target-state via inotify, update cache on change."""
    project_dir = find_project_dir()
    if not project_dir:
        return

    cache_file = _cache_key("statusline")
    lock_file = cache_file + ".lock"

    # Prevent duplicate daemons
    if os.path.exists(lock_file):
        try:
            old_pid = int(open(lock_file).read().strip())
            os.kill(old_pid, 0)
            return  # Already running
        except (ValueError, OSError):
            os.remove(lock_file)

    # Write lock
    with open(lock_file, "w") as f:
        f.write(str(os.getpid()))

    # Find state file to watch
    watch_dir = None
    for subdir in [".dut-serial", "mcp-rs/.dut-serial"]:
        d = Path(project_dir) / subdir
        if d.is_dir():
            watch_dir = d
            break

    if not watch_dir:
        os.remove(lock_file)
        return

    # Initial write (serial state only, model added by hook)
    serial_text = _compute_serial_text(project_dir)
    with open(cache_file, "w") as f:
        f.write(serial_text)

    # Setup inotify on the directory (watch for target-state modifications)
    try:
        fd = _inotify_init()
        _inotify_add_watch(fd, str(watch_dir), IN_CLOSE_WRITE | IN_MOVED_TO | IN_MODIFY)
    except OSError:
        os.remove(lock_file)
        return

    while True:
        try:
            r, _, _ = select.select([fd], [], [], 30)
            if not r:
                continue  # just wait, inotify will trigger on change

            # Read inotify events
            data = os.read(fd, 4096)
            for event in _parse_inotify_events(data):
                if "target-state" in event:
                    serial_text = _compute_serial_text(project_dir)
                    with open(cache_file, "w") as f:
                        f.write(serial_text)
                    break
        except (OSError, KeyboardInterrupt):
            break

    os.close(fd)
    try:
        os.remove(lock_file)
    except OSError:
        pass

def _parse_inotify_events(data):
    """Parse raw inotify event buffer, yield filenames."""
    i = 0
    while i + 16 <= len(data):
        wd, mask, cookie, name_len = struct.unpack_from("iIII", data, i)
        i += 16
        if name_len > 0 and i + name_len <= len(data):
            name = data[i:i+name_len].rstrip(b'\x00').decode('utf-8', errors='replace')
            i += name_len
            yield name


# ── Main (statusline hook entry) ─────────────────────────────────────────────

def main():
    if "--daemon" in sys.argv:
        if os.fork():
            sys.exit(0)
        os.setsid()
        run_daemon()
        sys.exit(0)

    # Read model from stdin (Claude Code passes JSON)
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

    # Right side: cached serial state (updated by inotify daemon)
    cache_file = _cache_key("statusline")
    serial_text = ""
    try:
        with open(cache_file, "r") as f:
            serial_text = f.read().strip()
    except (FileNotFoundError, OSError):
        pass

    # Cache miss: compute inline and start daemon
    project_dir = find_project_dir()
    if not serial_text and project_dir:
        serial_text = _compute_serial_text(project_dir)
        Popen([sys.executable, __file__, "--daemon"],
              stdout=DEVNULL, stderr=DEVNULL, start_new_session=True)

    if serial_text:
        print(f"{left}  {serial_text}")
    else:
        print(left)


if __name__ == "__main__":
    main()
