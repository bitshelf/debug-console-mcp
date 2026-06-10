#!/usr/bin/env python3
"""Shared utilities for embedded-debug hooks — fast (<2ms), zero network I/O."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
from typing import Optional


def find_project_dir() -> Optional[str]:
    """Check CWD for .target.conf. Only Claude Code project dir, no walk up."""
    if env := os.environ.get("TARGET_CONF"):
        p = Path(env)
        if p.exists():
            return str(p.resolve().parent)
    cwd = Path.cwd()
    if (cwd / ".target.conf").exists():
        return str(cwd)
    return None


def has_target_conf() -> bool:
    """Check if .target.conf exists in CWD (Claude Code project dir). No walk up."""
    return (Path.cwd() / ".target.conf").exists()


def get_session_id(project_dir: str) -> str:
    """Stable 8-char hex session ID from project directory path."""
    return hashlib.md5(str(Path(project_dir).resolve()).encode()).hexdigest()[:8]


def is_daemon_alive(session_id: str) -> bool:
    """Check if daemon process is running for the given session ID."""
    pid_file = Path(f"/tmp/embedded-debug/{session_id}/daemon.pid")
    if not pid_file.exists():
        return False
    try:
        pid = int(pid_file.read_text().strip())
        os.kill(pid, 0)  # signal 0 = existence check, no signal sent
        return True
    except (ValueError, OSError, ProcessLookupError):
        return False


def find_target_conflict(host: str, port: str, session_id: str) -> Optional[int]:
    """Check if another daemon (different session) already owns host:port.

    Scans /tmp/embedded-debug/ for other session dirs whose daemon holds
    the same ser2net target. Returns the conflicting daemon PID, or None.
    """
    base = Path("/tmp/embedded-debug")
    if not base.is_dir():
        return None
    for entry in base.iterdir():
        if not entry.is_dir() or entry.name == session_id:
            continue
        # Read target.conf from this session dir to compare host:port
        target_conf = entry / "target.conf"
        if not target_conf.exists():
            continue
        # Check if its daemon is actually alive
        pid_file = entry / "daemon.pid"
        if not pid_file.exists():
            continue
        try:
            pid = int(pid_file.read_text().strip())
            os.kill(pid, 0)
        except (ValueError, OSError, ProcessLookupError):
            continue  # stale session dir
        # Compare host:port from cached target.conf
        conf_host = conf_port = ""
        try:
            for line in target_conf.read_text().splitlines():
                line = line.strip()
                if line.startswith("DEV_HOST_IP="):
                    conf_host = line.split("=", 1)[1].strip().strip('"').strip("'")
                elif line.startswith("SERIAL_PORT="):
                    conf_port = line.split("=", 1)[1].strip().strip('"').strip("'")
        except OSError:
            continue
        if conf_host == host and conf_port == port:
            return pid
    return None


def read_state(session_id: str) -> Optional[str]:
    """Read target-state file from daemon runtime dir. Returns state string or None."""
    state_file = Path(f"/tmp/embedded-debug/{session_id}/target-state")
    if state_file.exists():
        try:
            return state_file.read_text().strip()
        except OSError:
            pass
    return None


def format_serial_state(state: str) -> Optional[tuple[str, str]]:
    """Format state string for statusline. Returns (display_text, color) or None.

    状态语义:
      - active:       启动完成，shell 就绪，可执行命令
      - booting:      启动中 (SPL 检测到，正在启动)
      - uboot:        U-Boot 交互模式
      - crashed:      内核崩溃 (panic/BUG/Oops)
      - disconnected: 连不上 dev host (ser2net 不可达)
      - DUT-off:      目标卡死，长时间无输出

    不显示的状态:
      - stopped:      MCP Server 未运行
      - connecting:   正在建立连接 (短暂过渡)
    """
    mapping = {
        # 启动完成，可执行命令
        "active": ("● serial:active", "green"),
        # 启动中
        "booting": ("◐ serial:booting", "yellow"),
        # U-Boot 交互模式
        "uboot": ("● serial:uboot", "cyan"),
        # 内核崩溃
        "crashed": ("✗ serial:crashed", "red"),
        # 连不上 dev host
        "disconnected": ("✗ serial:disconnected", "red"),
        # 目标卡死
        "DUT-off": ("✗ serial:DUT-off", "red"),
    }
    return mapping.get(state)


def check_mcp_alive(project_dir: str) -> bool:
    """Check if MCP server process is alive via PID file.

    Checks both .dut-serial/mcp.pid (Python daemon) and
    mcp-rs/.dut-serial/mcp.pid (Rust MCP server).
    """
    for subdir in [".dut-serial", "mcp-rs/.dut-serial"]:
        pid_file = Path(project_dir) / subdir / "mcp.pid"
        if not pid_file.exists():
            continue
        try:
            pid = int(pid_file.read_text().strip())
            os.kill(pid, 0)
            # Verify the process IS an embedded-debug server
            comm = (Path("/proc") / str(pid) / "comm").read_text().strip()
            if "python" in comm or "server" in comm or "embedded" in comm:
                return True
            cmdline = (Path("/proc") / str(pid) / "cmdline").read_text()
            if "embedded-debug" in cmdline or "server.py" in cmdline:
                return True
        except (ValueError, OSError, ProcessLookupError, FileNotFoundError):
            continue
    return False
