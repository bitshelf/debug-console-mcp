# Serial Debug Console — Embedded Linux DUT Serial Debugger

**CLI-first**: dutabo serial, state, reboot, uboot, maskrom, flash.
**MCP-optional**: All CLI commands also work through MCP tools.
**Triggers**: `/serial-debug` (Claude Code), `Use serial-debug` (Codex)

## Setup

```bash
# Install from GitHub Releases
curl -L https://github.com/bitshelf/debug-console-mcp/releases/latest/download/debug-console-mcp-$(uname -m) -o ~/.local/bin/debug-console-mcp
chmod +x ~/.local/bin/debug-console-mcp

# Verify
debug-console-mcp --version
dutabo list
```

## Configuration

Create `.target.toml` in the project root:

```toml
[[dev_hosts]]
alias = "my-pc"
ip = "192.168.1.100"
user = "linaro"

[[dut]]
alias = "my-board"
dev_host = "my-pc"

[dut.serial]
port = 2002

[dut.target]
login_user = "root"
```

## Quick Start

```bash
# List configured DUTs
dutabo list

# Check state
dutabo state -d my-board

# Interactive serial console (Ctrl-T q to quit)
dutabo serial -d my-board

# Software reboot
dutabo reboot -d my-board

# Enter U-Boot
dutabo uboot -d my-board

# Flash firmware
dutabo uf /path/to/update.img -d my-board
```

## MCP Tools (Claude Code uses these automatically)

| Tool | Purpose |
|------|---------|
| `serial_send_command` | Execute shell command on target |
| `serial_get_state` | Get current target state |
| `serial_get_logs` | Read serial logs (highlighted by default) |
| `serial_reset` | Hardware reset via relay |
| `serial_enter_uboot` | Enter U-Boot interactive prompt |
| `serial_enter_maskrom` | Enter Rockchip MASKROM mode |
| `serial_wait_pattern` | Wait for regex pattern in serial |
| `serial_uboot_command` | Send command at U-Boot prompt |
| `serial_flash_plan` | Plan firmware flash |
| `serial_flash` | Execute firmware flash |
| `serial_load_reference` | Load reference log for StageLearner |
| `serial_learn_connection` | Verify serial connectivity |

## StageLearner (Self-Learning Boot Detection)

The MCP server can auto-detect boot stages (DDR → SPL → U-Boot → Kernel → Shell) via text similarity:

1. Capture a reference boot log: `dutabo serial -d my-board` then `serial_reset(wait_boot=true)`
2. Load it: `serial_load_reference(reference_log_path=".dut-serial/my-board/reference-boot.log")`
3. Subsequent boots are automatically staged and monitored

## Troubleshooting

```bash
# Check MCP server health
curl http://127.0.0.1:3000/health

# Read highlighted logs
curl -X POST http://127.0.0.1:3000/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"serial_get_logs","arguments":{"lines":50}}}'
```
