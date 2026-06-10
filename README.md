# sermcp

Rust MCP server for embedded Linux DUT debugging. TCP direct to ser2net,
strsim-based boot stage detection, self-learning reference log, relay/PDU
power control, multi-DUT support, exponential backoff reconnect.

## Hardware Setup — Dev Host

### ser2net Configuration (no udev rules)

Serial access goes through ser2net on the dev host — its plain TCP-port →
serial-device mapping, nothing else. No udev rules are needed.

```yaml
# /etc/ser2net.yaml — port → serial device mapping (plain /dev/tty* paths)
connection: &con2000
    accepter: tcp,0.0.0.0,2000
    enable: on
    options:
      kickolduser: true          # prevent zombie connections
      telnet-brk-on-sync: true
    connector: serialdev,
              /dev/ttyACM0,
              115200n81,local                      # RK3576 default baud
```

## Configuration Reference (`.target.jsonc`)

JSONC (JSON with `//` comments). A DUT nests inside its dev host and inherits
`ip`/`user`/`pass` from the parent — names are display-only; the host ip is the linking key. Top-level
sections (`serial`/`relay`/`monitor`/...) are global fallbacks; a DUT value
overrides them. Unknown keys are rejected at parse time (typos fail loudly).

Full annotated schema: [references/.target.jsonc.example](references/.target.jsonc.example).
Create/update interactively with `dutabo init` — it preserves your comments.

### Single Board

```jsonc
{
  "dev_hosts": [
    {
      "ip": "192.168.1.105",
      "user": "linaro",
      "duts": [
        {
          "dut_name": "rk3576-board1",
          "serial": { "port": 2000 },
          "target": {
            "login_user": "root",
            "login_prompt": ""         // custom login regex (empty = default: `login:\s*$`)
          },
          "uboot": {
            "interrupt_char": "ctrl_c",
            "interrupt_strategy": "flood"
          },
          "relay": {
            "type": "usb-relay",
            "port": 2001,
            "reset_ch": 1,
            // "maskrom_ch": 2,
            "reset_time_ms": 3000      // minimum USB relay reset pulse (default: 3000)
          },
          "monitor": {
            "hang_timeout": 60,
            "boot_window_secs": 30,   // booting → active deadline (default 30)
            "max_archived_logs": 10,
            "reference_log": ".dut-serial/rk3576-board1/reference-boot.log",
            // StageLearner similarity thresholds (0.0–1.0). Defaults from Cargo.toml.
            "learner_stage_threshold": 0.45,   // boot stage classification
            "learner_crash_threshold": 0.50    // crash pattern detection
          },
          "flash": {
            "tool": "upgrade_tool",
            "upload_dir": "/tmp",
            "full_image_cmd": "uf {image}",
            "kernel_image_cmd": "di -k {image}",
            "loader_bin": "/path/to/MiniLoaderAll.bin",
            "loader_cmd": "db {loader}"
          }
        }
      ]
    }
  ]
}
```

### Multiple Boards

Add more entries to a host's `duts` array (or add another host). Each DUT gets:
- Independent `.dut-serial/<dut_name>/` directory
- Independent `target-state`, `statusline-cache`, logs
- Independent relay configuration

```jsonc
{
  "dev_hosts": [
    {
      "ip": "192.168.1.105",
      "user": "linaro",
      "duts": [
        {
          "dut_name": "rk3576-board1",
          "serial": { "port": 2000 }
          // ... config ...
        },
        {
          "dut_name": "rk3576-board2",
          "serial": { "port": 2008 },    // different port!
          "target": { "login_user": "root" },
          "monitor": { "reference_log": ".dut-serial/rk3576-board2/reference-boot.log" }
        }
      ]
    }
  ]
}
```

## CLI (`dutabo`)

`dutabo` is a manual CLI tool for interactive debug. Install separately:

```bash
# → installs dutabo to ~/.cargo/bin
cargo install --git https://github.com/bitshelf/sermcp
```

Commands:

```bash
dutabo init                    # interactive JSONC config wizard (also creates/merges the sermcp entry in .mcp.json)
dutabo list                    # DUT table (name/host/user@ip/port/state) from .target.jsonc
dutabo list --json             # same data as machine-readable JSON (inventory schema)
dutabo serial [--dut <dut_name>]  # interactive serial console
dutabo reset [--dut <dut_name>]   # hardware reset
dutabo uf <image> [--dut]      # flash firmware
dutabo uboot [--dut <dut_name>]   # enter U-Boot
dutabo status [--dut <board_name>] # show board status
```

`dutabo status` also prints the code-agent sessions registered for the
project, including each session ID, its owner/guest role, and start time.

Each project directory runs exactly one MCP process. Multiple code agents
share that engine through HTTP sessions: the first code-agent session can
operate the DUT, while later sessions can only call tools explicitly marked
read-only and read resources, prompts, and task state. Mutating tool calls and
task update/cancel requests return `agent_read_only`. `dutabo` remains the
human control path and does not consume the code-agent owner slot.

### Reference

- [rmcp docs](https://docs.rs/rmcp/latest/rmcp/) — official rmcp 3.x docs
- [rmcp-macros](https://docs.rs/rmcp-macros/latest/rmcp_macros/) — #[tool] macro reference
