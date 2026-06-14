# Debug Console MCP Server

Rust MCP server for embedded Linux DUT debugging. TCP direct to ser2net,
strsim-based boot stage detection, self-learning reference log, relay/PDU
power control, multi-DUT support, exponential backoff reconnect.

> Design spec: [docs/DESIGN-v0.3.md](docs/DESIGN-v0.3.md)
> Observability: `#[instrument]` tracing spans on all key async functions

## Quick Start — New Project

```bash
# 1. Build & deploy
cd mcp-rs && cargo build --release && ./deploy.sh

# 2. Create config + per-DUT directory
cp references/.target.toml.example .target.toml
vi .target.toml                            # set [[dev_hosts]].ip, [dut.serial].port, [dut].alias
mkdir -p .dut-serial/$(grep alias .target.toml | head -1 | cut -d'"' -f2)/logs

# 3. Restart Claude Code → SessionStart hook auto-starts MCP
```

## Hardware Setup — Dev Host

### udev Persistent Naming

Prevent `/dev/ttyACM*` renumbering across replug/reboot cycles by mapping each board's
USB serial number to a stable alias.

```bash
# On dev host, create /etc/udev/rules.d/99-rk3576-duts.rules:
# Use ATTRS{bInterfaceNumber} instead of KERNEL — kernel device numbers
# change across reboots. Interface 00 is the main console on dual-serial chips.
SUBSYSTEM=="tty", ATTRS{serial}=="<USB_SERIAL>", ATTRS{bInterfaceNumber}=="00", \
  SYMLINK+="serial/by-alias/<dut_alias>"

# Apply:
sudo udevadm control --reload-rules && sudo udevadm trigger

# Verify:
ls -la /dev/serial/by-alias/
```

**Finding a board's USB serial number:**
```bash
ssh <dev_host> "udevadm info --query=property --name=/dev/ttyACM0 | grep ID_SERIAL_SHORT"
```

### ser2net Configuration

```yaml
# /etc/ser2net.yaml — port → serial device mapping
connection: &con2000
    accepter: tcp,0.0.0.0,2000
    enable: on
    options:
      kickolduser: true          # prevent zombie connections
      telnet-brk-on-sync: true
    connector: serialdev,
              /dev/serial/by-alias/<dut_alias>,   # use udev symlink
              115200n81,local                      # RK3576 default baud
```

### Board Mapping Reference

| Alias | USB Serial | ser2net Port | OS |
|-------|-----------|-------------|-----|
| `rk3576-pdstars` | `5C2C244700` | 2000 | Yocto |
| `rk3576-yt9215` | `56E6019371` | 2008 | Ubuntu |

## Configuration Reference (`.target.toml`)

### Single Board

```toml
[[dev_hosts]]
alias = "rk-board-pc"
ip = "192.168.1.105"
user = "linaro"

[[dut]]
alias = "rk3576-pdstars"
dev_host = "rk-board-pc"

[dut.serial]
port = 2000

[dut.target]
login_user = "root"
login_prompt = ""         # custom login regex (empty = default: `login:\s*$`)

[dut.uboot]
interrupt_char = "ctrl_c"
interrupt_strategy = "aggressive"

[dut.relay]
type = "usb-relay"
port = 2001
reset_ch = 1
# maskrom_ch = 2
reset_time_ms = 3000      # minimum USB relay reset pulse (default: 3000)

[dut.monitor]
hang_timeout = 60
max_archived_logs = 10
reference_log = ".dut-serial/rk3576-pdstars/reference-boot.log"
# StageLearner similarity thresholds (0.0–1.0). Defaults from Cargo.toml.
learner_stage_threshold = 0.45   # boot stage classification
learner_crash_threshold = 0.50   # crash pattern detection

[dut.flash]
tool = "upgrade_tool"
upload_dir = "/tmp"
full_image_cmd = "uf {image}"
kernel_image_cmd = "di -k {image}"
loader_bin = "/path/to/MiniLoaderAll.bin"
loader_cmd = "db {loader}"
```

### Multiple Boards

Add additional `[[dut]]` blocks. Each DUT gets:
- Independent `.dut-serial/<alias>/` directory
- Independent `target-state`, `statusline-cache`, logs
- Independent relay configuration

```toml
# ... same dev_hosts ...

[[dut]]
alias = "rk3576-pdstars"
# ... config ...

[[dut]]
alias = "rk3576-yt9215"
dev_host = "rk-board-pc"

[dut.serial]
port = 2008    # different port!

[dut.target]
login_user = "root"

[dut.monitor]
reference_log = ".dut-serial/rk3576-yt9215/reference-boot.log"
```

## MCP Tools (28 total)

| Tool | Description |
|------|-------------|
| `serial_send_command` | Execute shell command on DUT |
| `serial_get_state` | Get state (active/booting/uboot/crashed/DUT-off/disconnected) |
| `serial_get_logs` | Retrieve serial logs (regex filter, streaming) |
| `serial_list_logs` | List archived boot logs |
| `serial_reset` | Hardware reset + log rotation |
| `serial_enter_uboot` | Enter U-Boot (Ctrl-C flood, bootdelay=0 compatible) |
| `serial_reboot_uboot` | Soft reboot + Ctrl-C flood -> U-Boot |
| `serial_enter_maskrom` | Enter MASKROM mode (if relay configured) |
| `serial_wait_pattern` | Wait for pattern (probe on timeout) |
| `serial_uboot_command` | Send command at U-Boot `=>` prompt |
| `serial_new_log` | Manually rotate log |
| `serial_poll_logs` | Incremental output (file position tracking) |
| `serial_get_config` | View current config (read-only) |
| `serial_get_metrics` | Engine metrics: uptime, command/error count, pending |
| `serial_claim` | Claim serial ownership for this session |
| `serial_button` | Press/release/pulse reset/maskrom/recovery buttons |
| `serial_pause` | Pause serial engine for dutabo takeover |
| `serial_resume` | Resume after pause |
| `serial_send_raw` | Send raw bytes (no markers, no wrapping) |
| `serial_load_reference` | Load reference log for StageLearner |
| `serial_get_stages` | View learned stage fingerprints |
| `serial_get_unclassified` | Get unclassified lines (self-learning) |
| `serial_append_reference` | Append anchor lines + hot-reload StageLearner |
| `serial_learn_connection` | Auto-learn connection (3x reset, similarity check) |
| `serial_verify_relay` | Verify CH340 relay control (ON/OFF read-back) |
| `serial_flash_plan` | Generate flash plan from .target.toml [flash] config |
| `serial_flash` | Execute firmware flash via dev host SSH |

## Target States

```
● serial:active        — DUT ready, login configured
◐ serial:booting       — U-Boot → kernel boot in progress
● serial:uboot         — U-Boot interactive prompt
✗ serial:crashed       — kernel panic / BUG / Oops detected
✗ serial:DUT-off       — no response (hang timeout)
✗ serial:disconnected  — ser2net unreachable
```

When `.target.toml` has multiple `[[dut]]` entries, statusline shows all:
```
● rk3576-pdstars:active  ● rk3576-yt9215:active
```

## StageLearner Tuning

The StageLearner uses text similarity (3-gram Jaccard + Jaro-Winkler) to
detect boot stages without SOC-specific regex. Two thresholds control
classification accuracy:

### `learner_stage_threshold` (default 0.45)

Minimum similarity score for normal boot stage classification.

| Range | Effect |
|-------|--------|
| 0.30–0.40 | Loose — may produce false stage detections |
| **0.45–0.55** | **Balanced (recommended)** |
| 0.60–0.80 | Strict — may miss valid stages |

### `learner_crash_threshold` (default 0.50)

Minimum similarity score for crash pattern detection. Set higher than
the stage threshold because crash lines are high-signal events that
warrant higher confidence.

### Example

```toml
[dut.monitor]
# Stricter matching for a noisy serial line:
learner_stage_threshold = 0.50
# Lower crash threshold to catch custom firmware panics:
learner_crash_threshold = 0.40
```

Defaults (0.45 / 0.50) are in `Cargo.toml` `[package.metadata.learn]`.

---

## Known Limitations & Fallbacks

### BusyBox ash Pipe Buffering

On Yocto/BusyBox targets, `echo ... | grep ...` often returns empty output
because ash's pipe buffering delays `grep` output past the exit-code marker.

| Pattern | Fallback |
|---------|----------|
| `echo <data> \| grep <pat>` | `printf '<data>\n' \| grep <pat>` |
| `echo <data> \| head -N` | 加 `; true` 同步: `echo <data> \| head -N; true` |
| `dmesg \| grep <pat>` | `dmesg \| grep <pat>; true` |
| Any pipe command | 加 `; true` 确保管道刷新完成 |

**The MCP returns a `hint` field in the JSON response when it detects this pattern.**

### First-Command Warmup

The first `serial_send_command` after MCP startup may return empty.
Always start with a warmup: `serial_send_command("echo warmup", timeout=3)`.

### USB Relay Minimum Reset Time

USB relays (CH340) require a minimum pulse duration to physically toggle.
Default: **3000ms** (3 seconds). Override in `.target.toml` via
`[dut.relay] reset_time_ms = 5000`.

### Reconnect Behavior

When the serial connection drops, the MCP uses exponential backoff:
**1s → 2s → 4s → 8s → 16s → cap at 30s**, resetting to 1s after 60s of
stable connection. The Agent is notified on each retry.

## Observability

All key async functions are instrumented with `#[tracing::instrument]`:

```
serial_send_command{cmd="uname -a", timeout=8}          ← MCP tool entry
  queue_command{command="uname -a"}                      ← engine dispatch
    execute{command="uname -a", timeout_secs=8}          ← command queue

start{host="192.168.1.105", target="2000"}               ← engine lifecycle
  probe_initial_state{result=active}                     ← initial probe

stop                                                    ← engine shutdown
```

Each span records duration (`time.busy`, `time.idle`) and structured fields
for querying by command text, exit code, timeout status.

View spans in real-time:
```bash
debug-console-mcp --log-to-stderr --verbose
```

## Hooks

| Hook | Trigger | Purpose |
|------|---------|---------|
| `session-start.py` | Enter project | Detect `.target.toml`, start MCP (HTTP mode) |
| `pre-tool-use.py` | Before Bash | Block raw `nc`/`tio`/`screen` serial access |
| `user-prompt-submit.py` | Before each prompt | Multi-DUT state alert, auto-restart MCP |
| `statusline.py` | ~1s refresh | Show DUT state(s) in statusline |

## Adding a New Board

1. **Find USB serial number:**
   ```bash
   ssh <dev_host> "udevadm info --query=property --name=/dev/ttyACM0 | grep ID_SERIAL_SHORT"
   ```

2. **Add udev rule** (on dev host, `/etc/udev/rules.d/99-rk3576-duts.rules`):
   ```
   SUBSYSTEM=="tty", ATTRS{serial}=="<serial>", ATTRS{idProduct}=="55d2", \
     KERNEL=="ttyACM0", SYMLINK+="serial/by-alias/<new_alias>"
   ```

3. **Add ser2net port** (on dev host, `/etc/ser2net.yaml`):
   ```yaml
   connection: &con<port>
       accepter: tcp,0.0.0.0,<port>
       enable: on
       options:
         kickolduser: true
       connector: serialdev,
                 /dev/serial/by-alias/<new_alias>,
                 115200n81,local
   sudo systemctl restart ser2net
   ```

4. **Create project `.target.toml`** with matching `serial.port` and `dut.alias`

5. **Create `.dut-serial/<alias>/logs/`** directory (or let MCP auto-create it)

6. **Start Claude Code** in the project directory → MCP auto-starts

## CLI (`dutabo`)

```bash
dutabo list                    # list DUTs from .target.toml
dutabo serial [--dut <alias>]  # interactive serial console
dutabo reset [--dut <alias>]   # hardware reset
dutabo uf <image> [--dut]      # flash firmware
dutabo uboot [--dut <alias>]   # enter U-Boot
dutabo state [--dut <alias>]   # get DUT state
```
