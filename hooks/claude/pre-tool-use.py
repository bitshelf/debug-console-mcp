#!/usr/bin/env python3
"""
PreToolUse hook — enforce Rust MCP for serial interaction.

Intercepts Bash commands that try to use nc/ncat/tio/screen/cu/stty
to interact with the target's serial port or relay, and BLOCKS them
(continue: false) so the agent must use MCP tools instead.
"""

import json
import re
import sys
from pathlib import Path

_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

from lib import find_project_dir


def _read_dev_host() -> str:
    """Read DEV_HOST_IP from project .target.toml, NOT CWD.
    Falls back to global lock files, then CWD walk-up."""
    # Use find_project_dir which checks lock files first (session-stable)
    proj = find_project_dir()
    search_dirs = [Path(proj)] if proj else [Path.cwd()]

    for base in search_dirs:
        toml_conf = base / ".target.toml"
        if toml_conf.exists():
            try:
                import tomllib
                with open(toml_conf, "rb") as f:
                    data = tomllib.load(f)
                # Check [[dut]] first, then [dev_host]
                for dut in data.get("dut", []):
                    dh = dut.get("dev_host", "")
                    if dh:
                        # resolve alias from [[dev_hosts]]
                        for host in data.get("dev_hosts", []):
                            if host.get("alias") == dh:
                                ip = host.get("ip", "")
                                if ip:
                                    return str(ip)
                # Check top-level [dev_host] and [[dev_hosts]]
                for hosts_key in ("dev_hosts",):
                    for host in data.get(hosts_key, []):
                        ip = host.get("ip", "")
                        if ip:
                            return str(ip)
                dh = data.get("dev_host", {})
                ip = dh.get("ip", "")
                if ip:
                    return str(ip)
            except (OSError, KeyError, ValueError, ImportError):
                pass

        conf = base / ".target.conf"
        if conf.exists():
            try:
                for line in conf.read_text().splitlines():
                    line = line.strip()
                    if line.startswith("DEV_HOST_IP="):
                        return line.split("=", 1)[1].strip().strip('"').strip("'")
            except OSError:
                pass
    return ""


# Patterns that indicate raw serial access — should use MCP instead
def _build_serial_patterns() -> list:
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
        r"dutabo\s+serial",              # dutabo interactive serial (human-only, Agent uses MCP)
        # Relay raw byte patterns: match literal \x prefix in shell strings.
        # In the command string, '\xa0' appears as backslash-x-a-0 (4 chars).
        # The regex \\x matches a literal backslash followed by 'x'.
        r"printf\s+.*\\x[0-9a-fA-F].*2000",  # relay raw packet to port 2000
        r"\\xa0\\x01",                       # relay protocol header bytes
    ]

# Patterns that are valid but should prefer MCP (built dynamically with dev host IP)
def _build_relay_patterns() -> list:
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
        print(json.dumps({"continue": True}))
        sys.exit(0)

    # ── CRITICAL: Block dangerous commands targeting the Dev Host ──
    # The Dev Host is a PRODUCTION machine (MYD-LR3576). It bridges serial
    # and runs upgrade_tool to flash the TARGET board via USB OTG.
    # NEVER reboot, flash, dd, rm -rf, or modify the Dev Host itself.
    dev_host = _read_dev_host()
    if dev_host:
        dev_host_pattern = re.escape(dev_host)
        dangerous = [
            # Only block reboot/shutdown of the dev host ITSELF, not target commands
            # like "reboot loader" or "reboot -f" which target the board via serial
            (rf"ssh\s+.*{dev_host_pattern}\s+(?:\"|')\s*(?:reboot|shutdown\s+now|poweroff)\s*(?:\"|')", "reboot/shutdown dev host"),
            (rf"ssh\s+.*{dev_host_pattern}.*(?:dd\s+if=|dd\s+of=/dev)", "dd disk write"),
            (rf"ssh\s+.*{dev_host_pattern}.*(?:mkfs\.|mke2fs|mkfs)", "mkfs format"),
            (rf"ssh\s+.*{dev_host_pattern}.*rm\s+-rf\s+/", "rm -rf /"),
            (rf"ssh\s+.*{dev_host_pattern}.*(?:apt\s+install|apt\s+remove|dpkg)", "apt/pkg modify"),
        ]
        for pattern, label in dangerous:
            if re.search(pattern, command):
                print(json.dumps({
                    "continue": False,
                    "systemMessage": (
                        f"[CRITICAL] BLOCKED: dangerous operation ({label}) targeting Dev Host ({dev_host}). "
                        "The Dev Host is a PRODUCTION machine — NEVER modify it. "
                        "To flash a target board: the image must be UPLOADED to the Dev Host, "
                        "then upgrade_tool flashes the TARGET via USB OTG. "
                        "Use serial_flash_plan + serial_flash MCP tools, or rk-build skill scripts."
                    )
                }))
                sys.exit(0)

    serial_patterns = _build_serial_patterns()
    relay_patterns = _build_relay_patterns()

    # Check for raw serial access — BLOCK (continue: false)
    for pattern in serial_patterns:
        if re.search(pattern, command):
            print(json.dumps({
                "continue": True,
                "systemMessage": (
                    "[MCP-ENFORCE] Blocked raw serial/relay access in Bash. "
                    "Use MCP tools instead: serial_send_command, serial_get_state, "
                    "serial_reset, serial_enter_uboot, serial_get_logs. "
                    "These tools handle connection management, locking, and logging automatically."
                )
            }))
            sys.exit(0)

    # Check for relay direct access — BLOCK
    for pattern in relay_patterns:
        if re.search(pattern, command):
            print(json.dumps({
                "continue": True,
                "systemMessage": (
                    "[MCP-ENFORCE] Blocked relay control via raw TCP. "
                    "Use serial_reset() or serial_enter_uboot() instead. "
                    "MCP handles the 4-byte relay protocol automatically."
                )
            }))
            sys.exit(0)

    # Check for Python scripts that bypass MCP — warn (don't block)
    if "burnin" in command.lower() and "python" in command.lower():
        print(json.dumps({
            "continue": True,
            "systemMessage": (
                "[MCP-ENFORCE] External burn-in script detected. "
                "Consider using MCP tools for burn-in testing: "
                "serial_enter_uboot + serial_send_command('boot') in a loop. "
                "MCP provides logging, state tracking, and relay control."
            )
        }))
        sys.exit(0)

    # ── BLOCK: dutabo serial — interactive serial console, human-only ──
    # The Agent must use MCP tools (serial_send_command, serial_get_state, etc.)
    # instead of taking over the serial port interactively.
    if re.search(r"dutabo\s+serial", command):
        print(json.dumps({
            "continue": False,
            "systemMessage": (
                "[BLOCKED] dutabo serial is restricted to human use only. "
                "The Agent must use MCP tools: serial_send_command, serial_get_state, "
                "serial_reset, serial_enter_uboot, serial_uboot_command. "
                "dutabo serial is an interactive console session that takes over "
                "the serial port (pauses the MCP engine) and is not suitable for "
                "automated Agent interaction."
            )
        }))
        sys.exit(0)

    print(json.dumps({"continue": True}))


if __name__ == "__main__":
    main()
