# DUT Control & Debug System — Design Specification

> **Version**: 0.3 (draft)
> **Date**: 2026-06-18
> **Status**: Design phase — supersedes the v0.2 architecture
> **Scope**: Full redesign of the embedded-debug MCP system, adding `dutabo` CLI,
> multi-DUT support, power-control abstraction, firmware-flash abstraction, and
> connection-learning bootstrap.

## 1. Overview

A unified system for controlling and debugging embedded Linux DUTs (Device Under
Test) from a developer workstation. The system has three layers:

```
┌──────────────────────────────────────────────────────────┐
│  Agent (Claude Code)          │  Developer (CLI)         │
│  MCP tools via stdio/HTTP     │  dutabo CLI tool         │
└──────────────┬────────────────┴──────────┬───────────────┘
               │                           │
          ┌────▼───────────────────────────▼────┐
          │     DUT Control Engine (Rust)       │
          │  • Serial console (TCP → ser2net)   │
          │  • Power control (relay/PDU/SW)     │
          │  • Boot stage detection (strsim)    │
          │  • Log rotation (per boot cycle)    │
          │  • Flash abstraction (per-SoC)      │
          │  • Multi-DUT registry               │
          └──────────────┬──────────────────────┘
                       TCP
          ┌──────────────▼──────────────────────┐
          │  Dev Host                           │
          │  ser2net (serial + relay)           │
          │  upgrade_tool / flash tools         │
          └──────────────┬──────────────────────┘
                    USB / UART
          ┌──────────────▼──────────────────────┐
          │  DUT (RK3576 / RK3588 / ...)        │
          └─────────────────────────────────────┘
```

### Design principles

1. **strsim text similarity first** — boot stage detection uses text similarity
   (Jaccard + Jaro-Winkler), not hardcoded regex. Regex only for crash detection
   and login/password prompts.
2. **Self-learning** — when StageLearner can't classify a line, it's collected
   for the Agent to analyze and append to the reference log. Hot-reload, no restart.
3. **Power-control abstraction** — relay (CH340), SNMP PDU, software reboot, and
   physical button simulation are interchangeable behind a `PowerControl` trait.
4. **Multi-DUT** — one `.target.toml` can define multiple DUTs with aliases.
   Both MCP tools and `dutabo` CLI can select which DUT to operate on.
5. **No Agent writes to `.target.toml`** — Agent can only read config.
   `.mcp.json` is auto-generated if missing.

---

## 2. Learning Process (Bootstrap)

### 2.1 Relay-based learning (preferred)

```
1. Press reset (relay ch=RESET_CHANNEL → ON)
   → Create new file: boot-learn-<timestamp>.log
2. Release reset (relay ch=RESET_CHANNEL → OFF)
   → Save serial output to the file
3. Repeat steps 1-2 three times
4. Compare the first 50 lines of the three files:
   - If similarity > 93% → keep the last file as reference-boot.log
   - If similarity < 10% → relay is not working, fall back to software reboot
```

### 2.2 Software-reboot learning (fallback, no relay)

```
1. Send "reboot" command to DUT
   → Create new file: boot-learn-<timestamp>.log (first line = "reboot")
2. Save serial output to the file
3. Repeat three times
4. Compare file contents:
   - If similarity > 93% → keep the last file as reference-boot.log
   - Connection is established
```

### 2.3 Connection verification

- If learning succeeds → connection is established, reference log is ready.
- If no relay and reference log already exists → send `reboot`, capture output,
  compare with reference. If similarity > 93% → connection established.
- If all attempts fail → report error, Agent handles next steps.

---

## 3. Power Control Abstraction

### 3.1 Trait

```rust
pub trait PowerControl: Send + Sync {
    /// Press a button (set pin low / turn relay ON / PDU on).
    async fn press(&mut self, button: Button) -> Result<(), PowerError>;

    /// Release a button (set pin high / turn relay OFF / PDU off).
    async fn release(&mut self, button: Button) -> Result<(), PowerError>;

    /// Pulse: press → delay → release.
    async fn pulse(&mut self, button: Button, delay_ms: u64) -> Result<(), PowerError>;

    /// Verify the control is actually working (read back state).
    async fn verify(&mut self) -> Result<bool, PowerError>;
}

pub enum Button {
    Reset,
    Recovery,
    Maskrom,
}
```

### 3.2 Implementations

| Implementation | Config keys | How it works |
|---------------|-------------|--------------|
| **Ch340Relay** | `relay.host`, `relay.port`, `relay.reset_ch`, `relay.maskrom_ch`, `relay.recovery_ch` | 4-byte TCP protocol `[0xA0, ch, op, cksum]` via ser2net. Verify: send command, read back, compare. |
| **SnmpPdu** | `pdu.host`, `pdu.community_v3`, `pdu.reset_oid`, ... | SNMP v3 SET/GET on OID. Future implementation. |
| **SoftwareReboot** | (none — uses serial) | Send `reboot` command over serial. No hardware control. |
| **ButtonSim** | `button.gpio_chip`, `button.gpio_line`, ... | GPIO line control via `/dev/gpiochipN`. Future. |

### 3.3 Config semantics

- `relay.reset_ch = 1` → Ch340Relay with reset on channel 1
- `# relay.maskrom_ch = 2` (commented) → MASKROM not controlled via relay
- `# relay.recovery_ch = 3` (commented) → Recovery not controlled via relay
- No `relay.*` section at all → fall back to SoftwareReboot

---

## 4. `.target.toml` Configuration

### 4.1 Format rules

1. First lines are comments (`#`).
2. `[serial]` — serial console TCP connection (IP + port).
3. `[relay]` or `[pdu]` or `[button]` — power control connection (IP + port + channels).
4. `[dev_host]` — dev host IP, username, login password.
5. If serial IP / power-control IP is empty → defaults to `dev_host.ip`.
6. `reference_log` — path to boot-stage reference log.
7. `[target]` — DUT serial login username + password (empty = no login needed).
8. Multiple DUTs: `[[dut]]` array with `alias`, `serial`, `relay`, etc.
9. `[flash]` — dev host flash tool name + per-SoC command arguments.

### 4.2 Full example (single DUT)

```toml
# Embedded Debug Target Configuration

[dev_host]
ip = "192.168.1.105"
user = "linaro"
# pass = ""          # commented = not set; "" = empty password

[serial]
# ip = ""            # empty → defaults to dev_host.ip
port = 2004

[relay]
# ip = ""            # empty → defaults to dev_host.ip
port = 2004
reset_ch = 1         # channel for RESET button
# maskrom_ch = 2     # commented = MASKROM not controlled
# recovery_ch = 3    # commented = Recovery not controlled

[target]
login_user = "root"
# login_pass = ""    # commented = not set

[uboot]
interrupt_char = "ctrl_c"   # "ctrl_c" for Rockchip, "2" for Allwinner
interrupt_strategy = "aggressive"

[monitor]
hang_timeout = 60
max_archived_logs = 10

[flash]
tool = "upgrade_tool"       # or "rkdeveloptool", "fastboot", ...
# Per-SoC flash commands (abstracted):
full_image_cmd = "uf {image}"          # flash full update.img
kernel_image_cmd = "wp kernel {image}" # flash kernel only
loader_bin = "loader.bin"              # MASKROM mode loader binary path
# scp upload target on dev host:
upload_dir = "/tmp"

# StageLearner reference log
reference_log = ".dut-serial/reference-boot.log"
```

### 4.3 Multi-DUT example

```toml
# Dev host with two DUTs connected

[dev_host]
ip = "192.168.1.105"
user = "linaro"

[[dut]]
alias = "rk3576-a"
serial.port = 2001
relay.port = 2001
relay.reset_ch = 1
relay.maskrom_ch = 2
target.login_user = "root"
reference_log = ".dut-serial/rk3576-a-reference.log"

[[dut]]
alias = "rk3588-b"
serial.port = 2002
relay.port = 2002
relay.reset_ch = 1
# maskrom_ch not set → no maskrom control for this DUT
target.login_user = "root"
reference_log = ".dut-serial/rk3588-b-reference.log"
```

### 4.4 Agent restrictions

- Agent **must not** generate or modify `.target.toml` — read only.
- If `.target.toml` exists but `.mcp.json` doesn't → SessionStart hook
  auto-generates `.mcp.json` and starts the serial MCP server.

---

## 5. `dutabo` CLI Tool

A standalone CLI tool for developers (inspired by LavaLabo). Lives on the dev
host or the developer workstation. Reads `.target.toml` for connection info.

### 5.1 Commands

```bash
# List available DUTs (from .target.toml)
dutabo list

# Connect to serial console (doesn't kick Agent's connection)
dutabo serial [--dut <alias>]

# Control power
dutabo reset [--dut <alias>]
dutabo power off [--dut <alias>]
dutabo power on [--dut <alias>]

# Flash firmware
dutabo uf <path/to/update.img> [--dut <alias>]     # flash full image
dutabo uf <path/to/boot.img> --part kernel [--dut] # flash specific partition

# Enter boot modes
dutabo uboot [--dut <alias>]    # enter U-Boot prompt
dutabo maskrom [--dut <alias>]  # enter MASKROM mode

# Get DUT state
dutabo state [--dut <alias>]
```

### 5.2 Flash workflow

```
dutabo uf path/to/update.img
  1. Resolve symlinks (if update.img is a symlink → use real path)
  2. Upload image to dev host /tmp/ (via scp)
  3. Verify upload integrity (checksum)
  4. If DUT in MASKROM → flash loader.bin first, then image
  5. If multiple DUTs in loader mode → prompt user to select
  6. Execute flash command (from [flash] section in .target.toml)
  7. Reset DUT and verify boot
```

### 5.3 Non-destructive serial access

`dutabo serial` shares the serial console with the MCP server **without kicking
the Agent's connection**. Implementation: connects to the same ser2net port;
ser2net allows multiple TCP connections to the same serial port (if configured
in `ser2net.conf` with `raw:0` mode). The MCP server's lock is cooperative
(file lock), not exclusive at the TCP level.

---

## 6. CH340 Relay Verification

```
1. Send control command (ON or OFF) to relay channel
2. Read back the relay state (STATUS command)
3. If read-back matches the sent command → relay is connected and controllable
4. If mismatch or timeout → relay not available, fall back to software reboot
```

Config required in `.target.toml`:
```toml
[relay]
ip = "192.168.1.105"   # or empty → defaults to dev_host.ip
port = 2001
reset_ch = 1
```

---

## 7. Boot Stage Detection (StageLearner)

### 7.1 Algorithm

**Primary**: `strsim` text similarity (Jaccard 3-gram 0.6 + Jaro-Winkler 0.4).
**Fallback**: regex for crash (panic/BUG/Oops) and login/password prompts only.

```rust
fn classify_line(&mut self, line: &str) -> Option<String> {
    let jaccard = jaccard_similarity(&line_grams, &fp.anchor_grams);
    let jaro = strsim::jaro_winkler(line, &fp.anchor);
    let score = jaccard * 0.6 + jaro * 0.4;
    // ... threshold filter, order constraint
}
```

### 7.2 Self-learning cycle

```
1. StageLearner can't classify a line → collected to unclassified.log
2. Agent calls serial_get_unclassified() → sees the lines
3. Agent identifies the boot stage (e.g. "this is DDR init for RK3576")
4. Agent calls serial_append_reference("DDR fdeec6f4fc typ...")
5. StageLearner hot-reloads → new fingerprints active immediately
6. Next boot cycle → DDR is correctly detected → log is split at the right point
```

### 7.3 BootStart (log split) triggers

| Stage | Triggers BootStart? | Why |
|-------|---------------------|-----|
| DDR | Yes | Earliest reboot signal — board restarts, DDR init first |
| SPL | Yes | Traditional boot start marker |
| U-Boot | Yes (if no DDR/SPL detected yet) | Fallback boot start |
| BL31/OP-TEE/kernel | No | Mid-boot stages, not restart signals |

`boot_detected` flag prevents duplicate splits within the same cycle.

---

## 8. Agent Integration

### 8.1 Auto-start

- SessionStart hook detects `.target.toml` → auto-generates `.mcp.json` →
  Claude Code spawns MCP server (stdio) or hook starts HTTP server.
- Agent **must not** write `.target.toml`.

### 8.2 MCP Tools

| Tool | Purpose |
|------|---------|
| `serial_send_command` | Execute shell command on DUT |
| `serial_get_state` | Get DUT state (active/booting/uboot/crashed/DUT-off/disconnected) |
| `serial_get_logs` | Retrieve serial logs with regex filter |
| `serial_list_logs` | List archived boot logs |
| `serial_reset` | Hardware reset + log rotation (with retry) |
| `serial_enter_uboot` | Enter U-Boot prompt (continuous Ctrl-C flood, bootdelay=0 compatible) |
| `serial_reboot_uboot` | Soft reboot + Ctrl-C flood to enter U-Boot |
| `serial_enter_maskrom` | Enter MASKROM mode (if relay configured) |
| `serial_wait_pattern` | Wait for pattern (with force_prompt_wait on timeout) |
| `serial_uboot_command` | Send command at U-Boot prompt |
| `serial_new_log` | Manually rotate log |
| `serial_poll_logs` | Incremental output retrieval |
| `serial_get_config` | View current config (read-only) |
| `serial_claim` | Claim serial ownership |
| `serial_load_reference` | Load reference log for StageLearner |
| `serial_get_stages` | View learned fingerprints |
| `serial_get_unclassified` | Get unclassified lines (for self-learning) |
| `serial_append_reference` | Append lines + hot-reload StageLearner |

### 8.3 Proactive notifications

When DUT state changes to `crashed` or `DUT-off`/`disconnected`:
- `UserPromptSubmit` hook reads `target-state` file and injects a `systemMessage`
  alerting the Agent before the next prompt is processed.
- Agent can then decide: reset, analyze crash logs, or report to user.

### 8.4 State machine

```
active → booting → uboot → booting → active     (normal reboot)
active → booting → active                        (fast boot)
active → crashed                                  (kernel panic)
booting → DUT-off                                 (hang timeout)
any → disconnected                                (ser2net unreachable)
disconnected → active                             (auto-reconnect)
```

---

## 9. File Layout

```
project-root/
├── .target.toml              # Target config (Agent read-only, user edits)
├── .mcp.json                 # Auto-generated by SessionStart hook
├── .dut-serial/
│   ├── target-state          # Current DUT state (atomic write)
│   ├── statusline-cache      # ANSI-formatted statusline text
│   ├── mcp.pid               # MCP server PID (liveness check)
│   ├── mcp.log               # MCP server log
│   ├── reference-boot.log    # StageLearner reference (auto-updated)
│   ├── unclassified.log      # Unclassified lines (for Agent self-learning)
│   └── logs/
│       ├── serial.current.log  # Current boot cycle
│       ├── serial.full.log     # Full continuous log (never truncated)
│       ├── boot-001_*.log      # Archived boot cycles
│       └── boot-002_*.log
├── mcp-rs/                   # Rust MCP server + dutabo CLI
│   ├── src/
│   │   ├── main.rs           # MCP server entry
│   │   ├── dutabo.rs         # dutabo CLI entry (binary)
│   │   ├── power_control.rs  # PowerControl trait + impls
│   │   ├── flash.rs          # Flash abstraction
│   │   ├── serial_engine.rs  # Core engine
│   │   ├── boot_detector.rs  # StageLearner (strsim) + crash regex
│   │   └── ...
│   └── Cargo.toml
├── hooks/claude/             # Claude Code hooks (Python)
│   ├── lib.py
│   ├── session-start.py
│   ├── pre-tool-use.py
│   ├── user-prompt-submit.py
│   └── statusline.py
├── references/               # Example configs
│   ├── .target.toml.example
│   └── .target.conf.example
└── docs/                     # Design + review documents
```

---

## 10. Migration from v0.2

| v0.2 | v0.3 |
|------|------|
| `mcp-python/` (legacy) | Deleted |
| `scripts/` (legacy) | Deleted |
| `statusline-watch` daemon | Deleted (MCP writes /dev/shm directly) |
| regex-first detection | StageLearner-first (strsim) |
| Static reference log | Self-learning (Agent appends + hot-reload) |
| Single DUT | Multi-DUT via `[[dut]]` array |
| Relay-only power control | `PowerControl` trait (relay/PDU/SW/button) |
| No CLI | `dutabo` CLI tool |
| No flash abstraction | `[flash]` config + `dutabo uf` command |
| `.target.conf` (shell) | `.target.toml` (TOML, preferred) |
| `RK_*` prefixed keys | No prefix (TOML sections) |

---

## 11. Open items (future)

- [ ] `dutabo` CLI implementation (Rust binary in same crate)
- [ ] `PowerControl` trait + Ch340Relay / SoftwareReboot implementations
- [ ] SNMP PDU implementation (`SnmpPdu`)
- [ ] GPIO button simulation (`ButtonSim`)
- [ ] Multi-DUT `[[dut]]` config parsing + registry
- [ ] Flash abstraction (`[flash]` section, `dutabo uf` command)
- [ ] Learning bootstrap (3× reset → similarity check → reference log)
- [ ] `dutabo serial` non-destructive console sharing
- [ ] CH340 relay read-back verification
- [ ] Proactive crash/DUT-off notification to Agent (hook-level)
