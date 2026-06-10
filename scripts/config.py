#!/usr/bin/env python3
"""Config loading for embedded-debug — shared by daemon, monitor, hooks, agents.

Parses shell-style .target.conf, walks up directory tree to find it,
generates SESSION_ID from project dir hash.
"""

import os
import re
import hashlib
from pathlib import Path

# ── Defaults ────────────────────────────────────────────────────────────────

DEFAULTS = {
    "RK_DEV_HOST_IP": "192.168.1.189",
    "RK_SERIAL_PORT": "2000",
    "RK_DEV_HOST_USER": "linaro",
    "RK_RESET_PORT": "0",
    # Login / boot
    "RK_LOGIN_USER": "root",
    "RK_LOGIN_PASS": "",
    "RK_BOOT_COMPLETE_PATTERN": r"login:",
    "RK_SHELL_PROMPT": r"[#\$]\s*$",
    # Monitoring
    "RK_HANG_TIMEOUT": "60",
    "RK_HANG_HYSTERESIS": "3",
    "RK_MAX_ARCHIVED_LOGS": "50",
    "RK_IDLE_WARN_SEC": "30",
    # Optional relay device
    "RK_RELAY_DEVICE": "",
    "RK_RELAY_BIN": "serial_relay",
    # MASKROM port
    "RK_MASKROM_PORT": "",
}


# ── Config loading ──────────────────────────────────────────────────────────


def _parse_shell_config(path: str) -> dict:
    """Parse a shell-style key=value file. Handles export, quotes, comments."""
    cfg = {}
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            # Strip 'export ' prefix
            line = re.sub(r"^export\s+", "", line)
            m = re.match(r'(\w+)=["\']?(.*?)["\']?\s*(?:#.*)?$', line)
            if m:
                k, v = m.group(1), m.group(2).strip().strip('"').strip("'")
                cfg[k] = v
    return cfg


def _find_config_path(explicit: str | None = None) -> str | None:
    """Find .target.conf: explicit path > env var > walk up from CWD."""
    if explicit and Path(explicit).exists():
        return explicit
    if env := os.environ.get("TARGET_CONF"):
        if Path(env).exists():
            return env
    d = Path.cwd()
    while True:
        candidate = d / ".target.conf"
        if candidate.exists():
            return str(candidate)
        if d.parent == d:
            break
        d = d.parent
    return None


def _session_id(project_dir: str) -> str:
    """Stable 8-char hex session ID from project directory path."""
    return hashlib.md5(project_dir.encode()).hexdigest()[:8]


def load_config(config_path: str | None = None) -> dict:
    """Load and merge config from file + defaults. Always sets SESSION_ID, LOG_DIR.

    Returns a plain dict — use cfg['KEY'] to access.
    """
    cfg = dict(DEFAULTS)

    path = _find_config_path(config_path)
    project_dir = str(Path(path).resolve().parent) if path else str(Path.cwd())
    session_id = _session_id(project_dir)

    cfg["_CONFIG_PATH"] = path
    cfg["_PROJECT_DIR"] = project_dir
    cfg["SESSION_ID"] = session_id
    cfg["LOG_DIR"] = str(Path(f"/tmp/embedded-debug/{session_id}/logs"))
    cfg["RUNTIME_DIR"] = str(Path(f"/tmp/embedded-debug/{session_id}"))

    if path:
        file_cfg = _parse_shell_config(path)
        cfg.update(file_cfg)

    return cfg
