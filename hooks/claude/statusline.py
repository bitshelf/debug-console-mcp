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

_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

from lib import find_project_dir, format_serial_state, _is_excluded

# ── Config ──────────────────────────────────────────────────────────────────
TMP_ROOT = os.environ.get("TMPDIR", os.environ.get("TMP", "/tmp"))
CACHE_DIR = "/dev/shm" if os.path.isdir("/dev/shm") and os.access("/dev/shm", os.W_OK) else TMP_ROOT
CACHE_TTL = 10


# ── Git branch (cached, async refresh) ─────────────────────────────────────

def _project_root() -> str:
    """Return the project root via per-hash lock (never changes with CWD)."""
    from lib import find_project_dir
    proj = find_project_dir()
    return proj if proj else str(Path.cwd())


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
        # Use subprocess with cwd and argument list — no shell injection.
        # The script writes the result to the cache file atomically.
        import subprocess
        script = (
            'BRANCH=$(git branch --show-current 2>/dev/null || git rev-parse --short HEAD 2>/dev/null || echo "?"); '
            'git diff --quiet 2>/dev/null && git diff --cached --quiet 2>/dev/null || BRANCH="$BRANCH*"; '
            'echo "$(date +%s) $BRANCH"'
        )
        proc = subprocess.Popen(
            ["bash", "-c", script],
            cwd=os.getcwd(),
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            start_new_session=True,
        )

        def _finish():
            try:
                out, _ = proc.communicate(timeout=5)
                if out:
                    with open(cache, "w") as f:
                        f.write(out.decode().strip())
            except (subprocess.TimeoutExpired, OSError):
                pass
            finally:
                try:
                    os.rmdir(lock_dir)
                except OSError:
                    pass

        # Run the finish in a thread so we don't block the hook.
        import threading
        threading.Thread(target=_finish, daemon=True).start()
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

def _dut_serial_dirs(project_dir: Path) -> list[Path]:
    """Return all per-DUT directories that have statusline-cache or target-state.

    The MCP server writes to per-DUT subdirectories (e.g. .dut-serial/rk3576/)
    when DUT_ALIAS is configured. For multi-DUT projects, all active DUT dirs
    are returned so the statusline shows every DUT's state.

    Falls back to root .dut-serial/ only when no per-DUT dirs are found
    (legacy single-DUT without alias).
    """
    base = project_dir / ".dut-serial"
    dirs: list[Path] = []
    if base.is_dir():
        for child in sorted(base.iterdir()):
            if child.is_dir() and (
                (child / "statusline-cache").exists()
                or (child / "target-state").exists()
            ):
                dirs.append(child)
        # Root cache can be newer than per-DUT caches when MCP server lacks DUT_ALIAS.
        # Replace stale per-DUT entries with root cache when root is fresher.
        root_cache = base / "statusline-cache"
        if root_cache.exists():
            root_mtime = root_cache.stat().st_mtime
            dirs = [d for d in dirs if not (
                (d / "statusline-cache").exists()
                and (d / "statusline-cache").stat().st_mtime < root_mtime
            )]
            if not dirs:
                dirs.append(base)
    return dirs


def _read_serial_text(project_dir: str) -> str:
    """Read ANSI-formatted serial state from all per-DUT MCP cache files.

    For multi-DUT projects, concatenates all DUT states separated by spaces.
    The MCP server writes pre-formatted ANSI text to per-DUT statusline-cache
    files; we read those directly. Falls back to formatting from target-state
    for legacy single-DUT without pre-formatted cache.
    """
    base = Path(project_dir) / ".dut-serial"
    dut_dirs = _dut_serial_dirs(Path(project_dir))
    texts: list[str] = []
    for dut in dut_dirs:
        cache = dut / "statusline-cache"
        if cache.exists():
            try:
                text = cache.read_text().strip()
                if text:
                    texts.append(text)
                    continue
            except OSError:
                pass
        state_file = dut / "target-state"
        if state_file.exists():
            try:
                state = state_file.read_text().strip()
                if state:
                    # Use DUT alias from directory name, falling back to "serial"
                    label = dut.name if dut != base else "serial"
                    formatted = format_serial_state(state, label)
                    if formatted:
                        display_text, color = formatted
                        ansi = {"green": "\033[32m", "red": "\033[31m",
                                "cyan": "\033[36m", "yellow": "\033[33m"}
                        code = ansi.get(color, "")
                        reset = "\033[0m" if code else ""
                        texts.append(f"{code}{display_text}{reset}")
            except OSError:
                pass
    return " ".join(texts)


def _cwd_has_target_toml() -> bool:
    """Only show serial state when CWD is within a debug-console project tree."""
    d = Path.cwd().resolve()
    for _ in range(10):
        if (d / ".target.toml").exists():
            return True
        if d.parent == d:
            break
        d = d.parent
    return False


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

    # Right side: serial state — from MCP server's project, verified via .target.toml
    # Skip entirely when CWD is under an excluded path (e.g. allwinner project)
    serial_text = ""
    if not _is_excluded(Path.cwd()):
        project_dir = find_project_dir()
        if project_dir and (Path(project_dir) / ".target.toml").exists():
            serial_text = _read_serial_text(project_dir)

    if serial_text:
        print(f"{left}  {serial_text}")
    else:
        print(left)


if __name__ == "__main__":
    main()
