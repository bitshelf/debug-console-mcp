# Debug Console MCP Server

Rust MCP server for embedded Linux DUT debugging. TCP direct to ser2net,
strsim-based boot stage detection, self-learning reference log, relay/PDU
power control, multi-DUT support.

> Design spec: [docs/DESIGN-v0.3.md](docs/DESIGN-v0.3.md)

## Quick Start

```bash
# 1. Build
cd mcp-rs && cargo build --release
./deploy.sh          # installs to ~/.local/bin/

# 2. Configure
cp references/.target.toml.example .target.toml
vi .target.toml      # set dev_host.ip, serial.port, relay channels

# 3. Launch Claude Code in the project directory
# SessionStart hook auto-detects .target.toml, generates .mcp.json,
# and starts the MCP server.
```

## Configuration (`.target.toml`)

```toml
[dev_host]
ip = "192.168.1.105"
user = "linaro"
# pass = ""          # commented = not set; "" = empty password

[serial]
port = 2004          # ser2net TCP port (ip defaults to dev_host.ip)

[relay]
port = 2004          # relay TCP port (ip defaults to dev_host.ip)
reset_ch = 1         # RESET button channel
# maskrom_ch = 2     # commented = not controlled
# recovery_ch = 3    # commented = not controlled

[target]
login_user = "root"
# login_pass = ""    # commented = not set

[uboot]
interrupt_char = "ctrl_c"       # "ctrl_c" Rockchip, "2" Allwinner
interrupt_strategy = "aggressive"

[monitor]
hang_timeout = 60
max_archived_logs = 10

# StageLearner reference log (enables text-similarity boot detection + log split)
reference_log = ".dut-serial/reference-boot.log"
```

**Config semantics**: commented (`#`) = not set; empty string (`""`) = explicitly
empty; Agent must not modify `.target.toml`.

## MCP Tools

| Tool | Description |
|------|-------------|
| `serial_send_command` | Execute shell command on DUT |
| `serial_get_state` | Get state (active/booting/uboot/crashed/DUT-off/disconnected) |
| `serial_get_logs` | Retrieve serial logs (regex filter, streaming) |
| `serial_list_logs` | List archived boot logs |
| `serial_reset` | Hardware reset + log rotation (retry, force_prompt_wait) |
| `serial_enter_uboot` | Enter U-Boot (continuous Ctrl-C flood, bootdelay=0 compatible) |
| `serial_reboot_uboot` | Soft reboot + Ctrl-C flood → U-Boot |
| `serial_enter_maskrom` | Enter MASKROM mode (if relay configured) |
| `serial_wait_pattern` | Wait for pattern (probe on timeout) |
| `serial_uboot_command` | Send command at U-Boot `=>` prompt |
| `serial_new_log` | Manually rotate log |
| `serial_poll_logs` | Incremental output (file position tracking) |
| `serial_get_config` | View current config (read-only) |
| `serial_claim` | Claim serial ownership |
| `serial_load_reference` | Load reference log for StageLearner |
| `serial_get_stages` | View learned fingerprints |
| `serial_get_unclassified` | Get unclassified lines (self-learning) |
| `serial_append_reference` | Append anchor lines + hot-reload StageLearner |

## Self-Learning Workflow

```
1. serial_get_unclassified()     → see lines StageLearner couldn't classify
2. Agent identifies the stage    → "this is DDR init for RK3576"
3. serial_append_reference(...)  → append anchor lines, hot-reload
4. Next boot cycle               → DDR correctly detected, log split at right point
```

## Transport Modes

| Mode | Config | Use case |
|------|--------|----------|
| **stdio** (default) | `"command": "debug-console-mcp"` | Claude Code spawns directly, low latency |
| **HTTP** | `debug-console-mcp --http 127.0.0.1:3000` | Independent process, `dutabo` CLI shares connection |

## Hooks

| Hook | Trigger | Purpose |
|------|---------|---------|
| `session-start.py` | Enter project | Detect `.target.toml`, start MCP (HTTP mode) |
| `pre-tool-use.py` | Before Bash | Block raw `nc`/`tio`/`screen` serial access → use MCP |
| `user-prompt-submit.py` | Before each prompt | Alert if DUT crashed/offline, auto-restart MCP |
| `statusline.py` | ~1s refresh | Show DUT state + git branch in statusline |

## Statusline

```
● serial:active        — DUT ready
◐ serial:booting       — booting (DDR → kernel)
● serial:uboot         — U-Boot interactive
✗ serial:crashed       — kernel panic/BUG/Oops
✗ serial:DUT-off       — no response (hang timeout)
✗ serial:disconnected  — ser2net unreachable
```

## CLI (`dutabo`) — planned

```bash
dutabo list                    # list DUTs from .target.toml
dutabo serial [--dut <alias>]  # serial console (doesn't kick Agent)
dutabo reset [--dut <alias>]   # hardware reset
dutabo uf <image> [--dut]      # flash firmware
dutabo uboot [--dut <alias>]   # enter U-Boot
dutabo state [--dut <alias>]   # get DUT state
```

See [docs/DESIGN-v0.3.md](docs/DESIGN-v0.3.md) for full design.
