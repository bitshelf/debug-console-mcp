---
name: embedded-debug
description: >-
  Debug an embedded Linux DUT through the debug-console MCP only when the
  current project or one of its parent directories contains .target.jsonc.
  Use for boot-log capture, U-Boot control, crash diagnosis, relay reset,
  heartbeat monitoring, and adaptive boot-stage detection. Do not use this
  skill in projects without .target.jsonc.
---

# Embedded Debug

Before using any serial tool, locate `.target.jsonc` from the current working
directory upward. If it is absent, stop: this plugin is intentionally inactive
for that project. Never create a configuration implicitly.

Treat `.target.jsonc` as user-owned, read-only configuration. Use the MCP tools
for target interaction; do not bypass the managed transport with `nc`, `tio`,
`screen`, or a second serial client.

Start with `serial_get_state` and `serial_get_config`. For commands, require the
marker-delimited result returned by `serial_send_command`; a queued local write
alone is not proof that the DUT received it. Before reset, U-Boot entry, or
flash operations, confirm the selected DUT and current ownership state.

Common flows:

- Boot failure: `serial_get_state`, then `serial_get_logs` around panic, oops,
  timeout, or disconnect lines.
- Target command: `serial_send_command`, then verify output and exit status.
- Reset: `serial_reset`, then monitor state/logs until the requested terminal
  state is reached.
- U-Boot: `serial_enter_uboot`, then use `serial_uboot_command`.
- Stale endpoint: inspect the reported owner PID, then use `serial_claim`; it
  never steals a lock from a live owner.

Do not terminate `debug-console-mcp` processes globally. Any process cleanup
must be limited to a PID proven to belong to this project and endpoint.
