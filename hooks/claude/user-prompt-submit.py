#!/usr/bin/env python3
"""
UserPromptSubmit hook — alert agent if target serial state needs attention.

Reads .dut-serial/target-state, outputs {"continue": true} or {"systemMessage": "..."}.
"""

import json
import os
import sys
from pathlib import Path

_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

from lib import find_project_dir, check_mcp_alive


def _read_ser2net_host(project_dir: str) -> tuple[str, str]:
    """从 .target.conf 读取 ser2net 地址."""
    conf = Path(project_dir) / ".target.conf"
    if not conf.exists():
        return ("", "")
    host = port = ""
    try:
        for line in conf.read_text().splitlines():
            line = line.strip()
            if line.startswith("DEV_HOST_IP="):
                host = line.split("=", 1)[1].strip().strip('"').strip("'")
            elif line.startswith("SERIAL_PORT="):
                port = line.split("=", 1)[1].strip().strip('"').strip("'")
    except OSError:
        pass
    return (host, port)


def _check_ser2net(host: str, port: str) -> bool:
    """TCP 连通性检查 ser2net 是否可达."""
    import socket
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(3)
        s.connect((host, int(port)))
        s.close()
        return True
    except (ValueError, ConnectionRefusedError, OSError):
        return False


def _restart_mcp(project_dir: str) -> None:
    """释放端口并重启 MCP HTTP server."""
    import subprocess
    import shutil
    binary = shutil.which("embedded-debug-mcp") or os.path.expanduser("~/.local/bin/embedded-debug-mcp")
    if not Path(binary).exists():
        return
    subprocess.run(["fuser", "-k", "3000/tcp"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.Popen(
        [binary, "--http"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True, cwd=project_dir,
    )


def read_target_state(project_dir: str) -> str:
    """Read current target state from either daemon location."""
    for subdir in [".dut-serial", "mcp-rs/.dut-serial"]:
        sf = Path(project_dir) / subdir / "target-state"
        if sf.exists():
            try:
                state = sf.read_text().strip()
                if state:
                    return state
            except OSError:
                pass
    return ""


def main():
    project_dir = find_project_dir()
    if not project_dir:
        print(json.dumps({"continue": True}))
        sys.exit(0)

    state = read_target_state(project_dir)

    # PID liveness check: if MCP server is dead, state file is stale
    if state and not check_mcp_alive(project_dir):
        state = "disconnected"

    if not state or state == "stopped":
        print(json.dumps({
            "systemMessage": (
                "[TARGET] MCP serial server is not running. "
                "Call any MCP tool (e.g. serial_get_state) to start it."
            )
        }))
        sys.exit(0)

    if state.startswith("DUT-off"):
        print(json.dumps({
            "systemMessage": (
                "[TARGET-ALERT] DUT-off — no serial output for extended period. "
                "Try serial_send_command(\"echo ping\") or serial_reset()."
            )
        }))
        sys.exit(0)

    if state == "disconnected":
        # 检查 ser2net 是否可达
        host, port = _read_ser2net_host(project_dir)
        ser2net_alive = _check_ser2net(host, port) if host and port else False
        if ser2net_alive:
            # dev host ser2net 正常 → 问题在本地 MCP → 自动重启
            _restart_mcp(project_dir)
            print(json.dumps({
                "systemMessage": (
                    "[TARGET] ser2net OK, MCP reconnected."
                )
            }))
        else:
            print(json.dumps({
                "systemMessage": (
                    "[TARGET-ALERT] Serial connection lost — ser2net on dev host unreachable."
                )
            }))
        sys.exit(0)

    if state == "crashed":
        print(json.dumps({
            "systemMessage": (
                "[TARGET-ALERT] Kernel crash detected! "
                "Run serial_get_logs(pattern=\"panic|BUG|Oops|Call trace\") to see details."
            )
        }))
        sys.exit(0)

    print(json.dumps({"continue": True}))

if __name__ == "__main__":
    main()
