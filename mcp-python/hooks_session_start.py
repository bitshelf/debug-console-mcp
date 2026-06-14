#!/usr/bin/env python3
"""SessionStart hook — auto-start daemon for projects with .target.conf.

Non-blocking: launches daemon in background, exits immediately.
"""

import os
import sys
import time
from pathlib import Path
from subprocess import Popen, DEVNULL

_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

try:
    from lib import find_project_dir, get_session_id, is_daemon_alive, find_target_conflict
except ImportError:
    from hooks_lib import find_project_dir, get_session_id, is_daemon_alive, find_target_conflict

DAEMON_SCRIPT = os.path.expanduser(
    "~/.claude/skills/embedded-debug/scripts/daemon.py"
)


def _parse_config(config_path: str, key: str) -> str:
    """Extract a value from a shell-style key=value config file.
    Returns empty string if key is not found."""
    import re
    try:
        with open(config_path) as f:
            for line in f:
                line = line.strip()
                if line.startswith("#") or "=" not in line:
                    continue
                line = re.sub(r"^export\s+", "", line)
                m = re.match(rf'{key}=["\']?(.*?)["\']?\s*(?:#.*)?$', line)
                if m:
                    return m.group(1).strip().strip('"').strip("'")
    except OSError:
        pass
    return ""


def _has_mcp_server(project_dir: str) -> bool:
    """Check if .mcp.json has embedded-debug MCP server configured.
    If yes, the MCP server handles serial — don't start the old daemon."""
    import json
    mcp_json = Path(project_dir) / ".mcp.json"
    if not mcp_json.exists():
        return False
    try:
        cfg = json.loads(mcp_json.read_text())
        return "embedded-debug" in cfg.get("mcpServers", {})
    except (json.JSONDecodeError, OSError):
        return False


def main():
    project_dir = find_project_dir()
    if not project_dir:
        sys.exit(0)

    # If v4 MCP server is configured, skip old daemon entirely
    if _has_mcp_server(project_dir):
        sys.exit(0)

    config_path = os.path.join(project_dir, ".target.conf")
    session_id = get_session_id(project_dir)

    if is_daemon_alive(session_id):
        sys.exit(0)

    host = _parse_config(config_path, "RK_DEV_HOST_IP")
    port = _parse_config(config_path, "RK_SERIAL_PORT")
    if host and port:
        conflict_pid = find_target_conflict(host, port, session_id)
        if conflict_pid is not None:
            sys.exit(0)

    env = os.environ.copy()
    env["EMBEDDED_DEBUG_NOFORK"] = "0"

    try:
        Popen(
            ["uv", "run", DAEMON_SCRIPT, f"--config={config_path}"],
            env=env,
            stdout=DEVNULL,
            stderr=DEVNULL,
            start_new_session=True,
        )
    except (FileNotFoundError, OSError) as e:
        log_dir = Path(f"/tmp/embedded-debug/{session_id}")
        log_dir.mkdir(parents=True, exist_ok=True)
        (log_dir / "start-error.log").write_text(str(e))
        sys.exit(0)

    time.sleep(0.5)
    sys.exit(0)


if __name__ == "__main__":
    main()
