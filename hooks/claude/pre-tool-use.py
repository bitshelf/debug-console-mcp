#!/usr/bin/env python3
"""
PreToolUse hook — enforce Rust MCP for serial interaction.

Intercepts Bash commands that try to use nc/ncat/tio/screen/cu/stty
to interact with the target's serial port or relay, and reminds the
agent to use MCP tools instead.
"""

import json
import re
import sys
import os
from pathlib import Path

_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

from lib import find_project_dir


def _read_dev_host() -> str:
    """Read DEV_HOST_IP from .target.conf. Returns empty string if not found."""
    conf = Path.cwd() / ".target.conf"
    if not conf.exists():
        return ""
    try:
        for line in conf.read_text().splitlines():
            line = line.strip()
            if line.startswith("DEV_HOST_IP="):
                return line.split("=", 1)[1].strip().strip('"').strip("'")
    except OSError:
        pass
    return ""


# Patterns that indicate raw serial access — should use MCP instead
def _build_serial_patterns() -> list[str]:
    """Build serial raw-access patterns, using the actual dev host IP if available."""
    dev_host = _read_dev_host()
    host_pattern = re.escape(dev_host) if dev_host else r"192\.168\.\d+\.\d+"
    return [
        rf"nc\s+.*{host_pattern}",       # netcat to dev host
        rf"ncat\s+.*{host_pattern}",
        r"tio\s+/dev/tty",               # tio serial terminal
        r"screen\s+/dev/tty",            # screen serial
        r"cu\s+-l\s+/dev/tty",           # cu serial
        r"stty\s+-F\s+/dev/tty",         # stty config
        r"picocom\s+/dev/tty",
        r"minicom\s+/dev/tty",
        r">\s*/dev/tty",                 # direct write to serial
        r"echo.*>\s*/dev/tty",
        r"socat\s+.*tty",                # socat to serial
        r"printf\s+.*\\\\x[0-9a-fA-F].*2000",  # relay raw packet
        r"\\\\xa0\\\\x01",               # relay protocol raw bytes
    ]

# Patterns that are valid but should prefer MCP (built dynamically with dev host IP)
def _build_relay_patterns() -> list[str]:
    """Build relay raw-access patterns, using the actual dev host IP if available."""
    dev_host = _read_dev_host()
    host_pattern = re.escape(dev_host) if dev_host else r"192\.168\.\d+\.\d+"
    return [
        rf"nc\s+.*{host_pattern}\s+2001",  # relay port
        r">\s*/dev/tcp/.*2001",
    ]


def main():
    try:
        hook_input = json.loads(sys.stdin.read())
    except (json.JSONDecodeError, OSError):
        print(json.dumps({"continue": True}))
        sys.exit(0)

    tool_name = hook_input.get("tool_name", "")
    tool_input = hook_input.get("tool_input", {})

    # Only intercept Bash commands
    if tool_name != "Bash":
        print(json.dumps({"continue": True}))
        sys.exit(0)

    command = tool_input.get("command", "")
    if not command:
        print(json.dumps({"continue": True}))
        sys.exit(0)

    project_dir = find_project_dir()
    if not project_dir:
        # No .target.conf found — not a embedded debug project
        print(json.dumps({"continue": True}))
        sys.exit(0)

    serial_patterns = _build_serial_patterns()
    relay_patterns = _build_relay_patterns()

    # Check for raw serial access
    for pattern in serial_patterns:
        if re.search(pattern, command):
            print(json.dumps({
                "continue": True,
                "systemMessage": (
                    "[MCP-ENFORCE] Detected raw serial/relay access in Bash. "
                    "Use MCP tools instead: serial_send_command, serial_get_state, "
                    "serial_reset, serial_enter_uboot, serial_get_logs. "
                    "These tools handle connection management, locking, and logging automatically."
                )
            }))
            sys.exit(0)

    # Check for relay direct access
    for pattern in relay_patterns:
        if re.search(pattern, command):
            print(json.dumps({
                "continue": True,
                "systemMessage": (
                    "[MCP-ENFORCE] Relay control via raw TCP detected. "
                    "Use serial_reset() or serial_enter_uboot() instead. "
                    "MCP handles the 4-byte relay protocol automatically."
                )
            }))
            sys.exit(0)

    # Check for Python scripts that bypass MCP
    if "burnin" in command.lower() and ("python" in command.lower() or "py" in command):
        print(json.dumps({
            "continue": True,
            "systemMessage": (
                "[MCP-ENFORCE] External burn-in script detected. "
                "Use MCP tools for burn-in testing instead: "
                "serial_enter_uboot + serial_send_command('boot') in a loop. "
                "MCP provides logging, state tracking, and relay control."
            )
        }))
        sys.exit(0)

    print(json.dumps({"continue": True}))


if __name__ == "__main__":
    main()
