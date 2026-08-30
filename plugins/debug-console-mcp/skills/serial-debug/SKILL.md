---
name: serial-debug
description: >-
  Use debug-console-mcp and dutabo for an embedded DUT only when the current
  project or a parent directory contains .target.jsonc. Covers serial state,
  commands, reboot, U-Boot, buttons, and flash planning. Do not trigger in
  projects without .target.jsonc.
---

# Serial Debug Console

First search upward from the current directory for `.target.jsonc`. When it is
not present, report that the plugin is inactive and do not start or configure a
serial connection.

Prefer MCP tools for agent-driven operations. `dutabo serial` is reserved for
explicit human interactive takeover. Never open the configured ser2net port
with raw terminal tools while the MCP owns it.

Use this order:

1. Call `serial_get_state` and confirm the DUT alias.
2. Read recent output with `serial_get_logs`.
3. Use `serial_claim` only for a released or stale endpoint lock.
4. Send the smallest diagnostic command and require target echo/output.
5. Perform reset, U-Boot, button, or flash actions only after the target and
   expected effect are clear.

The configured `uboot.interrupt_char` is the byte sent to the target;
`ctrl_c` is the symbolic value for `0x03`, not a separate action.
