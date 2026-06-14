---
name: embedded-debug
description: >-
  Embedded Linux serial debugger: boot log capture, U-Boot control, kernel crash
  detection, relay reset, heartbeat monitoring, adaptive stage detection. Use for
  serial console, target commands, reboot, burn-in testing.
---

# Embedded Debug — Rust MCP Serial Debugger v0.3

Rust 实现 (mcp-rs/), 零框架 MCP (纯 JSON-RPC 2.0)。strsim 文本相似度阶段检测 + 自学习参考日志。
支持 stdio + HTTP 双传输模式。设计文档: [docs/tech-design.md](docs/tech-design.md)。

## New in v0.3

- **Multi-DUT support**: per-DUT state files under `.dut-serial/<alias>/`, driven by `.target.toml` `[[dut]]` sections. Each DUT gets its own serial port, relay channel, and state tracking.
- **Exponential backoff reconnect**: 1s initial delay, doubling to 30s max on repeated connection failures. Prevents log spam and CPU churn when the serial port is unavailable.
- **Project-switch isolation**: per-hash lock directories (`/tmp/debug-console-<hash>.lock`) ensure multiple Claude sessions in different projects don't collide.
- **New tools**: `serial_button` (relay/power button control), `serial_get_stages` (inspect StageLearner fingerprints), `serial_get_unclassified` (retrieve unclassified boot lines), `serial_append_reference` (Agent-assisted self-learning).
- **Hardware Setup**: see [README.md](../README.md) for wiring diagrams, relay pinout, and ser2net configuration.

## CRITICAL: Dev Host vs Target Device

- **Dev Host** (`dev_host.ip`): 运行 ser2net 的中转机器，串口线和继电器接在这台机器上
- **Target Device** (DUT): 被调试的嵌入式板子，通过串口连接到 Dev Host
- **Local Machine**: 运行 Claude Code 的开发机，通过 MCP 与 Dev Host 上的 ser2net 通信
- `serial_send_command` 发送的命令在 **Target Device** 上执行，不是 Dev Host
- `serial_reset` 控制的是 Dev Host 上的继电器，从而复位 Target Device

## Setup

```bash
# 1. 在项目根目录创建 .target.toml (推荐)
cp ~/.local/share/debug-console-mcp/references/.target.toml.example .target.toml
# 编辑 [[dev_hosts]], [[dut]], [dut.serial], [dut.target] 等配置段

# 2. 确保 MCP binary 已安装
ls ~/.local/bin/debug-console-mcp

# 3. 启动 session 后，HTTP MCP 自动启动，statusline 读取缓存状态
# 无需手动操作 — SessionStart hook 处理一切
```

配置文件优先从 CWD 向上查找 `.target.toml`，legacy `.target.conf` 仅用于兼容旧项目。可用 `TARGET_CONF` 环境变量覆盖路径。

## Quick Reference

```
# 串口交互
serial_send_command "uname -a"
serial_get_state
serial_claim                 # 夺取串口所有权
serial_wait_pattern "login:" timeout=120

# 日志
serial_get_logs lines=50 pattern="error|panic"
serial_list_logs
serial_poll_logs             # 增量获取新输出

# 硬件控制
serial_reset wait_boot=true  # 继电器复位
serial_send_command "reboot" # 软重启 (<500ms, timeout=5)
serial_enter_uboot           # 进入 U-Boot 命令行 (硬件复位 + Ctrl-C flood)
serial_reboot_uboot          # 软重启 + Ctrl-C flood 进入 U-Boot (bootdelay=0 也有效)
serial_uboot_command "boot"  # 从 U-Boot 继续启动
serial_enter_maskrom         # 进入 Rockchip MASKROM 模式
serial_new_log               # 手动切日志 (不复位)
serial_button "reset"        # 继电器按钮控制 (reset/maskrom/recovery)

# 跨 SOC 自适应 (StageLearner)
serial_load_reference "/path/to/new-soc-boot.log"
serial_get_stages

# 自学习 (Agent 辅助)
serial_get_unclassified           # 获取未分类行
serial_append_reference lines="DDR fdeec6f4fc typ 23/09/25..."  # 追加锚点 + 热重载
```

## Target State

| State | Meaning | Trigger |
|-------|---------|---------|
| `active` | 启动完成，可执行命令 | Shell 提示符检测到 |
| `booting` | 启动中 | SPL 检测到，正在启动 |
| `uboot` | U-Boot 交互模式 | U-Boot 提示符检测到 |
| `crashed` | 内核崩溃 | panic/BUG/Oops 检测到 |
| `disconnected` | 连不上 dev host | ser2net 连接失败 |
| `DUT-off` | 目标卡死 | 启动中超时，长时间无输出 |

## Agent Self-Test

After code changes, verify:

```
serial_send_command("reboot")       → <3s return {"output":"reboot sent"}
serial_reset(wait_boot=false)       → <3s return {"success":true}
serial_enter_uboot                  → relay reset + Ctrl-C → U-Boot
serial_uboot_command("boot")        → continue boot from U-Boot
```

Monitor: `watch -n 0.5 cat .dut-serial/target-state`
State flow: `active → booting → uboot → booting → active`

## Configuration (.target.toml)

TOML 格式（推荐）。Agent **禁止修改**此文件，只能读。

```toml
[[dev_hosts]]
alias = "rk-board-pc"
ip = "192.168.1.xxx"
user = "linaro"
# pass = ""          # 注释 = 不设置；"" = 密码为空

[[dut]]
alias = "rk3576"
dev_host = "rk-board-pc"

[dut.serial]
port = 2000          # ser2net TCP port (ip 默认同 dev_host.ip)

[dut.target]
login_user = "root"
# login_pass = ""    # 注释 = 不设置

[dut.uboot]
interrupt_char = "ctrl_c"       # "ctrl_c" Rockchip, "2" Allwinner
interrupt_strategy = "aggressive"

[dut.relay]
# port = 2001        # 注释 = 无继电器（回退软件 reboot）
# reset_ch = 1       # RESET 通道
# maskrom_ch = 2     # 注释 = MASKROM 不控制
# recovery_ch = 3    # 注释 = Recovery 不控制
# power_ch = 4       # 可选：整机断电/上电通道

[dut.monitor]
hang_timeout = 60
max_archived_logs = 10

# StageLearner: 参考启动日志（启用文本相似度阶段检测 + 日志分割）
reference_log = ".dut-serial/rk3576/reference-boot.log"
```

## Common Workflows

### Boot Monitoring
```
serial_reset(wait_boot=false)
serial_wait_pattern("U-Boot SPL", timeout=30)
serial_wait_pattern("Linux version", timeout=60)
serial_wait_pattern("login:", timeout=120)
# Auto-login sends credentials automatically
serial_send_command("cat /proc/device-tree/model")
```

### Crash Diagnosis
```
serial_get_state  # → {state: "crashed"}
serial_get_logs(lines=200, pattern="panic|BUG|Oops|Call trace")
```

### U-Boot Interaction
```
serial_enter_uboot
serial_uboot_command("version")
serial_uboot_command("mmc list")
```

### Boot Cycle Comparison
```
serial_list_logs
serial_get_logs(archive=1, pattern="error|fail")  # previous boot
serial_get_logs(archive=0, pattern="error|fail")  # current boot
```

### 🆕 New SOC Debugging (StageLearner + Auto-Learning)

**方式一: 配置自动加载 (推荐)**

在 `.target.toml` 中设置参考日志路径，MCP 启动时自动加载：

```toml
[dut.monitor]
reference_log = ".dut-serial/rk3576/reference-boot.log"
```

**方式二: 手动加载**

```
# 1. 采集一份完整启动日志作为参考
serial_reset(wait_boot=true)   # 冷启动抓完整日志

# 2. 加载参考日志，启用自适应检测
serial_load_reference("/path/to/boot-001_xxx.log")

# 3. 检查已学习阶段
serial_get_stages
# → ddr:35, spl:14, bl31:42, optee:91, uboot:8, kernel:15, ...
```

**🔄 Agent 自学习工作流（StageLearner 未覆盖的阶段）**

当 StageLearner 遇到未知启动模式时，Agent 可辅助扩展指纹：

```
# 1. Agent 获取 StageLearner 未能分类的行
serial_get_unclassified
# → {"lines": ["DDR fdeec6f4fc typ 23/09/25...", "LP4/4x derate en..."], "count": 15}

# 2. Agent 分析后裁剪关键锚点行（避免时间戳/内存地址/随机数）
# 选择在每次启动中必然出现且顺序固定的行

# 3. Agent 追加到参考日志 + 热重载（无需重启 MCP）
serial_append_reference lines="DDR fdeec6f4fc typ 23/09/25..."

# 4. 下次 boot cycle 自动使用新指纹 → DDR 被正确检测
# 指纹数增加 → serial_get_stages 可见
```

**StageLearner 检测流程（v0.3.0+）：**

```
串口数据 → strip banner + strip ANSI
  ├─ 1. StageLearner 优先（组合分数: Jaccard*0.6 + Jaro-Winkler*0.4）
  │     ├─ 匹配成功 → BootEvent::Stage + (ddr/spl → BootStart)
  │     └─ 匹配失败 → 收集到 unclassified_lines
  ├─ 2. login/password regex（始终执行，需要触发登录动作）
  ├─ 3. Regex 回退（仅当 StageLearner 未匹配时）
  └─ 4. 未分类行 → 每 20 行或阶段边界 → 写入 unclassified.log
```

## Known Limitations & Fallbacks

### BusyBox `ash` pipe buffering

On Yocto/BusyBox targets, `echo ... | grep ...` often returns empty output
because ash doesn't flush the pipe before the exit-code line runs.

**When `serial_send_command` returns `"output":""` and `"exit_code":null`, retry
with one of these fallbacks:**

| Pattern | Replace with |
|---------|-------------|
| `echo <data> \| grep <pat>` | `printf '<data>\n' \| grep <pat>` |
| `echo <data> \| head -N` | `printf '<data>\n' \| head -N` |
| `dmesg \| head -N` | `dmesg \| head -N; true`（追加 `; true` 同步） |
| `cat /proc/xxx \| head` | 直接用 `head -N /proc/xxx`（跳过 pipe） |

```bash
# ❌ 可能返回空
serial_send_command("echo pipetest | grep pipe")

# ✅ 回退方案
serial_send_command("printf 'pipetest\n' | grep pipe")

# ✅ 或追加 ; true
serial_send_command("echo pipetest | grep pipe; true")
```

### First-command warmup

The first `serial_send_command` after engine start may return empty.
**Always run a warmup: `serial_send_command("echo warmup", timeout=3)` before
any real work.** If the warmup fails, retry once — the second attempt always
succeeds.

## Transport Modes

| Mode | Config | Use Case |
|------|--------|----------|
| **stdio** (默认) | `"type":"stdio"` | Claude Code 直接 spawn，低延迟 |
| **HTTP** (备用) | `debug-console-mcp --http` | 独立进程，端口 3000 |

stdio 模式的 MCP Server 由 Claude Code SessionStart hook 自动启动。
如果进程意外终止，重启会话即可恢复。
