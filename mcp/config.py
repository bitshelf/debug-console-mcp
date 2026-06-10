#!/usr/bin/env python3
"""Config loading — parses shell-style .target.conf, walks up directory tree."""

import os
import re
from pathlib import Path

DEFAULTS = {
    # ── 串口连接 ──
    "RK_DEV_HOST_IP": "192.168.1.189",
    "RK_SERIAL_PORT": "2000",
    "RK_SERIAL_PROTOCOL": "raw",
    "RK_SERIAL_BAUDRATE": "1500000",
    # ── 自动登录 ──
    "RK_LOGIN_USER": "root",
    "RK_LOGIN_PASS": "",
    # ── 继电器 ──
    "RK_RELAY_PORT": "0",
    "RK_RESET_CHANNEL": "0",
    "RK_MASKROM_CHANNEL": "0",
    # ── 监控 ──
    "RK_HANG_TIMEOUT": "60",
    "RK_HANG_HYSTERESIS": "3",
    # ── 日志 ──
    "RK_MAX_ARCHIVED_LOGS": "10",
    "RK_MAX_LOG_FILE_SIZE": "100",
    "RK_DUT_DIR": ".dut-serial",
    # ── U-Boot ──
    "RK_UBOOT_INTERRUPT_STRATEGY": "lava",
    # ── 全局锁目录 (跨项目) ──
    "RK_LOCK_DIR": "/tmp/embedded-debug/locks",
}


def _parse_shell_config(path: str) -> dict:
    """Parse a shell-style key=value file. Handles export, quotes, comments."""
    cfg = {}
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            line = re.sub(r"^export\s+", "", line)
            # 精确匹配: "双引号值" | '单引号值' | 无引号值
            m = re.match(r"""(\w+)=(?:"([^"]*?)"|'([^']*?)'|([^\s#]*))\s*(?:#.*)?$""", line)
            if m:
                k = m.group(1)
                v = next(g for g in (m.group(2), m.group(3), m.group(4)) if g is not None)
                cfg[k] = v
    return cfg


def find_config_path() -> str | None:
    """Find .target.conf: TARGET_CONF env var > walk up from CWD."""
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


def find_project_dir() -> str | None:
    """Walk up from CWD to find .target.conf. Returns project dir path."""
    path = find_config_path()
    return str(Path(path).resolve().parent) if path else None


def load_config(config_path: str | None = None) -> dict:
    """Load and merge config from file + defaults."""
    cfg = dict(DEFAULTS)
    path = config_path or find_config_path()
    if path:
        cfg["_CONFIG_PATH"] = path
        cfg["_PROJECT_DIR"] = str(Path(path).resolve().parent)
        file_cfg = _parse_shell_config(path)
        cfg.update(file_cfg)
    return cfg
