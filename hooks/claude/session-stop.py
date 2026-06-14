#!/usr/bin/env python3
"""SessionStop hook — minimal: clean session-local sentinel files only.

The MCP server owns all state (target-state, statusline-cache) and writes
both atomically. No daemon to clean up — just remove session sentinels.
"""

import os
import sys
import hashlib
from pathlib import Path

_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

from lib import find_project_dir


def main():
    project_dir = find_project_dir()
    if not project_dir:
        sys.exit(0)

    h = hashlib.md5(str(Path(project_dir).resolve()).encode()).hexdigest()[:8]
    cache_root = "/dev/shm" if os.path.isdir("/dev/shm") and os.access("/dev/shm", os.W_OK) else os.environ.get("TMPDIR", "/tmp")

    # Clean git cache lock dir (may be stale)
    lock_dir = Path(f"{cache_root}/claude-{h}-git.cache.lock")
    try:
        if lock_dir.is_dir():
            lock_dir.rmdir()
    except OSError:
        pass

    sys.exit(0)


if __name__ == "__main__":
    main()
