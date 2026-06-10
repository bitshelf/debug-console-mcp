# pi.dev — sermcp Client Bridge

pi.dev deliberately ships **without built-in MCP support** (see
`docs/usage.md`: "It intentionally does not include built-in MCP ... You can
build or install those workflows as extensions or packages"). This extension
is the missing client side: it spawns the `sermcp` binary (stdio
mode), performs the MCP handshake, and registers every `serial_*` tool via
`pi.registerTool()` so pi can drive embedded DUTs exactly like Claude Code.

```
pi (agent)  ──►  pi.registerTool("serial_get_state") ──┐
                                                       │ stdio JSON-RPC
                       pi-extensions/sermcp │ (newline-delimited)
                                                       ▼
                                       sermcp binary (Rust MCP server)
                                                       │
                                       ser2net (TCP) ──► DUT serial console
```

## Install

The repo root is a pi package (`package.json` `pi` manifest). From the repo
root:

```bash
pi install .
# or, from anywhere: pi install /path/to/sermcp
```

The extension is loaded from the package manifest
(`pi.extensions: ["./pi-extensions/sermcp"]`). Restart pi or run
`/reload` after installing.

## Binary discovery

First hit wins:

1. `$SERMCP_BIN` — explicit override
2. project `.mcp.json` → `mcpServers["sermcp"].command` (with `${VAR}` expansion)
3. `PATH` lookup `sermcp`
4. `~/.local/bin/sermcp` — standard install location
5. project `bin/sermcp`
6. package-local `bin/sermcp` — plugin tarball layout
   (`scripts/package-pi-plugin.sh` puts the binary here)

If no binary is found, only `serial_mcp_diagnostics` is registered; the
agent can call it to see the discovery result and install hint.

## Tools

On `session_start` the extension spawns the server and registers all tools
reported by `tools/list` (28 tools on v0.4.1): `serial_send_command`,
`serial_get_state`, `serial_get_logs`, `serial_reset`, `serial_enter_uboot`,
`serial_wait_pattern`, `serial_uboot_command`, `serial_flash_plan`,
`serial_set_baud`, ... plus the always-registered `serial_mcp_diagnostics`.

- Server `inputSchema` is passed straight through as the TypeBox
  `parameters` schema (all 28 schemas verified to compile with TypeBox).
- `executionMode: "sequential"` — serial operations on a shared port must
  not race.
- MCP `isError` responses throw, so pi renders them as tool failures.
- If the server exits, the next tool call respawns it lazily.

## Lifecycle

| Event | Action |
|-------|--------|
| `session_start` | resolve project dir + binary, spawn server, handshake, register tools |
| `session_shutdown` | kill server, reject in-flight requests |
| tool call (server dead) | respawn lazily with original binary/cwd |

Logs go to `.dut-serial/pi-mcp-client.log` in the project dir.

## Troubleshooting

```bash
# Is the bridge live? (returns binary, project dir, running flag, tool list)
pi -p "Call serial_mcp_diagnostics and report its output verbatim."

# Server-side log (Rust tracing)
tail -f .dut-serial/mcp.log

# Bridge-side log
tail -f .dut-serial/pi-mcp-client.log
```
