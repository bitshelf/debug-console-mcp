---
name: embedded-debug
description: >-
  Embedded Linux serial debugger: boot log capture, U-Boot control, kernel crash
  detection, relay reset, heartbeat monitoring, adaptive stage detection. Use for
  serial console, target commands, reboot, burn-in testing.
---

# Embedded Debug — Rust MCP Serial Debugger v0.2

Rust 实现 (mcp-rs/), 零框架 MCP (纯 JSON-RPC 2.0)。支持 stdio + HTTP 双传输模式。

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
serial_enter_uboot           # 进入 U-Boot 命令行
serial_uboot_command "boot"  # 从 U-Boot 继续启动
serial_new_log               # 手动切日志 (不复位)

# 🆕 跨 SOC 自适应 (StageLearner)
serial_load_reference "/path/to/new-soc-boot.log"
serial_get_stages
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

## Configuration (.target.conf)

```bash
RK_DEV_HOST_IP=192.168.1.xxx
RK_SERIAL_PORT=2000

# Optional auto-login
RK_LOGIN_USER=root
RK_LOGIN_PASS=password

# Optional relay control (via ser2net, no SSH)
RK_RELAY_PORT=2001
RK_RESET_CHANNEL=2
RK_MASKROM_CHANNEL=1

# Monitoring
RK_HANG_TIMEOUT=60
RK_MAX_ARCHIVED_LOGS=10

# StageLearner: 参考启动日志 (启动时自动加载)
RK_REFERENCE_LOG=/path/to/reference-boot.log
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
serial_send_command("version")
serial_send_command("mmc list")
```

### Boot Cycle Comparison
```
serial_list_logs
serial_get_logs(archive=1, pattern="error|fail")  # previous boot
serial_get_logs(archive=0, pattern="error|fail")  # current boot
```

### 🆕 New SOC Debugging (StageLearner)

**方式一: 配置自动加载 (推荐)**

在 `.target.conf` 中设置参考日志路径, MCP 启动时自动加载:

```bash
RK_REFERENCE_LOG=/path/to/reference-boot.log
```

**方式二: 手动加载**

```
# 1. 采集一份完整启动日志作为参考
serial_reset(wait_boot=true)   # 冷启动抓完整日志

# 2. 加载参考日志，启用自适应检测
serial_load_reference("/path/to/boot-001_xxx.log")
# → 提取 4000+ 指纹锚定点

# 3. 检查已学习阶段
serial_get_stages
# → ddr:35, spl:14, bl31:42, optee:91, uboot:8, kernel:15, ...

# 4. 后续所有串口自动用自适应模式
# 无需为每个新 SOC 写正则表达式
```

## Transport Modes

| Mode | Config | Use Case |
|------|--------|----------|
| **stdio** (默认) | `"type":"stdio"` | Claude Code 直接 spawn，低延迟 |
| **HTTP** (备用) | `embedded-debug-mcp --http` | 独立进程，端口 3000 |

stdio 模式的 MCP Server 由 Claude Code SessionStart hook 自动启动。
如果进程意外终止，重启会话即可恢复。
