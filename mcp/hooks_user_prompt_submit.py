#!/usr/bin/env python3
"""
UserPromptSubmit hook — alert agent if target serial state needs attention.

Reads .dut-serial/target-state, outputs {"continue": true} or {"systemMessage": "..."}.
"""

import json
import os
import sys
from pathlib import Path

# Ensure hooks directory is on sys.path for import
_HOOK_DIR = Path(__file__).resolve().parent
if str(_HOOK_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOK_DIR))

try:
    from lib import find_project_dir
except ImportError:
    from hooks_lib import find_project_dir


def main():
    project_dir = find_project_dir()
    if not project_dir:
        print(json.dumps({"continue": True}))
        sys.exit(0)

    state_file = Path(project_dir) / ".dut-serial" / "target-state"
    if not state_file.exists():
        print(json.dumps({"continue": True}))
        sys.exit(0)

    try:
        state = state_file.read_text().strip()
    except OSError:
        print(json.dumps({"continue": True}))
        sys.exit(0)

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
        print(json.dumps({
            "systemMessage": (
                "[TARGET-ALERT] Serial connection lost. "
                "Check Dev Host and ser2net, then call any MCP tool to reconnect."
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
