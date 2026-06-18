#!/usr/bin/env python3
"""
UserPromptSubmit hook — alert agent if target serial state needs attention.

Reads .dut-serial/target-state, outputs {"continue": true} or {"systemMessage": "..."}.
"""

import json
import os
import signal
import sys
from pathlib import Path

_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

from lib import find_project_dir, check_mcp_alive, _is_embedded_server


def _read_target_conf(project_dir: str) -> dict:
    """Read DEV_HOST_IP, SERIAL_PORT, and MCP_PORT from .target.conf."""
    conf = Path(project_dir) / ".target.conf"
    result = {}
    if not conf.exists():
        return result
    try:
        for line in conf.read_text().splitlines():
            line = line.strip()
            if line.startswith("DEV_HOST_IP="):
                result["host"] = line.split("=", 1)[1].strip().strip('"').strip("'")
            elif line.startswith("SERIAL_PORT="):
                result["port"] = line.split("=", 1)[1].strip().strip('"').strip("'")
    except OSError:
        pass
    return result


def _read_mcp_port(project_dir: str) -> int:
    """Read HTTP port from .mcp.json. Returns 3000 as default."""
    mcp_json = Path(project_dir) / ".mcp.json"
    if not mcp_json.exists():
        return 3000
    try:
        cfg = json.loads(mcp_json.read_text())
        url = cfg.get("mcpServers", {}).get("embedded-debug", {}).get("url", "")
        if ":" in url:
            return int(url.rsplit(":", 1)[-1].split("/")[0])
    except (json.JSONDecodeError, ValueError, KeyError):
        pass
    return 3000


def _check_ser2net(host: str, port: str) -> bool:
    """TCP connectivity check — ser2net reachable?"""
    import socket
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(2)  # 2s (was 3s — reduce user wait)
        s.connect((host, int(port)))
        s.close()
        return True
    except (ValueError, ConnectionRefusedError, OSError):
        return False


def _restart_mcp(project_dir: str) -> None:
    """Release the MCP HTTP port and restart the server.
    Only kills processes verified to be embedded-debug-mcp."""
    import subprocess
    import shutil
    binary = shutil.which("embedded-debug-mcp") or os.path.expanduser("~/.local/bin/embedded-debug-mcp")
    if not Path(binary).exists():
        return

    mcp_port = _read_mcp_port(project_dir)

    # Targeted kill: only kill embedded-debug processes on the MCP port.
    try:
        result = subprocess.run(
            ["fuser", f"{mcp_port}/tcp"], capture_output=True, text=True, timeout=5
        )
        if result.returncode == 0 and result.stdout.strip():
            for pid_str in result.stdout.strip().split():
                try:
                    pid = int(pid_str)
                    # Verify the process is an embedded-debug server before killing.
                    if _is_embedded_server(pid):
                        os.kill(pid, signal.SIGTERM)
                except (ValueError, OSError, ProcessLookupError):
                    pass
    except (subprocess.TimeoutExpired, OSError):
        pass

    subprocess.Popen(
        [binary, "--http", f"127.0.0.1:{mcp_port}"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True, cwd=project_dir,
    )


def read_target_state(project_dir: str) -> str:
    """Read current target state from .dut-serial/target-state."""
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
        # Check if ser2net is reachable
        conf = _read_target_conf(project_dir)
        host = conf.get("host", "")
        port = conf.get("port", "")
        ser2net_alive = _check_ser2net(host, port) if host and port else False
        if ser2net_alive:
            # ser2net OK → problem is local MCP → auto-restart
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
