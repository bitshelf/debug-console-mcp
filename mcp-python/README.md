# Embedded Debug MCP — 嵌入式串口调试工具

基于 MCP 协议的嵌入式 Linux 目标板串口调试工具。通过 Dev Host ser2net 连接目标板，
pyserial 直连（无 socat），提供启动日志捕获、自动登录、崩溃检测、U-Boot 中断、
继电器复位等功能。

## 架构概览

```
Build Machine                          Dev Host
┌──────────────────────┐              ┌──────────────────────┐
│ Claude Code           │              │ ser2net               │
│  ┌─────────────────┐  │   TCP:2000  │ TCP:2000 → /dev/ttyUSB0│
│  │ MCP Server      ├──┼────────────→│ (目标板串口)           │
│  │ (stdio, 子进程)  │  │   TCP:2001  │ TCP:2001 → /dev/ttyUSB1│
│  │ pyserial 直连    │  ├────────────→│ (CH340 继电器)         │
│  └─────────────────┘  │              └──────────────────────┘
└──────────────────────┘
```

## 快速开始

### 1. 前提

| 组件 | 说明 |
|------|------|
| Dev Host | 运行 ser2net，暴露目标板串口 (TCP:2000) 和继电器 (TCP:2001) |
| Python ≥ 3.11 | 构建机器上安装 |
| `uv` | Python 包管理器 |

### 2. 配置

在 SDK 项目根目录创建 `.target.conf`:

```bash
# 必填: 串口连接
RK_DEV_HOST_IP=192.168.1.xxx
RK_SERIAL_PORT=2000

# 可选: 自动登录
RK_LOGIN_USER=root
RK_LOGIN_PASS=mypassword

# 可选: 继电器复位 (直连 ser2net，无需 SSH)
RK_RELAY_PORT=2001
RK_RESET_CHANNEL=2
RK_MASKROM_CHANNEL=1
```

Claude Code 启动时自动检测 `.target.conf` → 自动生成 `.mcp.json` → 启动 MCP Server。

### 3. 基本用法

启动后在 Claude Code 中直接使用 MCP Tools:

```
# 查状态
serial_get_state
→ {state: "booted", boot_number: 3, ...}

# 向目标发命令 (仿 labgrid marker 模式)
serial_send_command "uname -a"

# 看日志
serial_get_logs(lines=50, pattern="error|panic")
serial_list_logs  # 列出所有启动日志

# 等待启动完成
serial_wait_pattern "login:" timeout=120

# 硬件复位
serial_reset(wait_boot=true)

# 进入 U-Boot 提示符
serial_enter_uboot
```

## MCP Tools 参考

| Tool | 功能 |
|------|------|
| `serial_send_command` | 在目标上执行 shell 命令 (marker echo 模式) |
| `serial_get_state` | 获取目标状态和元数据 |
| `serial_get_logs` | 检索串口日志 (支持过滤) |
| `serial_list_logs` | 列出所有归档启动日志 |
| `serial_reset` | 硬件复位 + 日志切割 |
| `serial_enter_uboot` | 强制进入 U-Boot 交互提示符 |
| `serial_wait_pattern` | 阻塞等待串口输出中出现指定模式 |
| `serial_new_log` | 手动切割日志 (不复位) |
| `serial_poll_logs` | 增量获取新输出 (长轮询) |
| `serial_get_config` | 获取当前目标配置 |

## 状态指示器

Statusline 显示当前目标状态:

| 显示 | 含义 | 触发条件 |
|------|------|---------|
| `● serial:active` | 启动完成，可执行命令 | Shell 提示符检测到 |
| `◐ serial:booting` | 启动中 | SPL 检测到，正在启动 |
| `● serial:uboot` | U-Boot 交互模式 | U-Boot 提示符检测到 |
| `✗ serial:crashed` | 内核崩溃 | panic/BUG/Oops 检测到 |
| `✗ serial:disconnected` | 连不上 dev host | ser2net 连接失败 |
| `✗ serial:DUT-off` | 目标卡死 | 启动中超时，长时间无输出 |
| (不显示) | MCP Server 未运行 | — |

## 日志

启动日志存储在项目根目录的 `.dut-serial/logs/` 下:

```
{project}/.dut-serial/
├── .boot_count            # 当前 cycle 编号
├── target-state           # 当前状态
└── logs/
    ├── boot-001_20260610_140500.log
    ├── boot-002_20260610_143200.log
    └── serial.current.log → boot-002_*.log
```

- 一次上电→掉电 = 一个 `boot-NNN.log`
- 默认保留最近 10 个日志
- 单文件超过 100MB 自动切割
- 检测 U-Boot SPL 自动触发切割

## 多 Claude Code 实例

每 Claude Code 实例启动自己的 MCP Server。同 host:port 互斥:

```
Claude A → MCP Server A → host:2000  ✅
Claude B → MCP Server B → host:2000  ❌ 被拒绝 (PID xxx already owns this target)
Claude C → MCP Server C → host:2001  ✅ 不同端口，允许
```

A 退出后释放锁 → B 可以重试。

## 配置参考

### `.target.conf`

```bash
# ── 串口连接（必填）──
RK_DEV_HOST_IP=192.168.1.xxx
RK_SERIAL_PORT=2000
RK_SERIAL_PROTOCOL=raw          # "raw" | "rfc2217"
RK_SERIAL_BAUDRATE=1500000

# ── 自动登录（可选）──
RK_LOGIN_USER=root
RK_LOGIN_PASS=

# ── 继电器控制（可选）──
RK_RELAY_PORT=2001
RK_RESET_CHANNEL=2
RK_MASKROM_CHANNEL=1

# ── 监控参数 ──
RK_HANG_TIMEOUT=60              # 判定挂死的无输出秒数 (默认 60)
RK_HANG_HYSTERESIS=3            # 滞后确认次数 (默认 3)
RK_MAX_ARCHIVED_LOGS=10         # 保留日志数 (默认 10)
RK_MAX_LOG_FILE_SIZE=100        # 单文件最大 MB (默认 100)

# ── U-Boot 中断策略 ──
# RK_UBOOT_INTERRUPT_STRATEGY=aggressive  # "lava" (默认) | "aggressive"
```

## 典型工作流

### 监控启动

```
serial_reset(wait_boot=false)
serial_wait_pattern("U-Boot SPL", timeout=30)
serial_wait_pattern("Linux version", timeout=60)
serial_wait_pattern("login:", timeout=120)
# 自动登录后
serial_send_command("cat /proc/device-tree/model")
```

### 崩溃诊断

```
serial_get_state
# → {state: "crashed"}
serial_get_logs(lines=200, pattern="panic|BUG|Oops|Call trace")
```

### U-Boot 交互

```
serial_enter_uboot
serial_send_command("version")
serial_send_command("mmc list")
serial_send_command("printenv bootcmd")
```

### 日志审查

```
serial_list_logs
# 对比两次启动
serial_get_logs(archive=1, pattern="error|fail")  # 上一次
serial_get_logs(archive=0, pattern="error|fail")  # 当前
```

## 故障排除

| 现象 | 原因 | 解决 |
|------|------|------|
| 状态栏不显示 | 无 `.target.conf` 或 Server 未启动 | 检查项目根目录是否有 `.target.conf` |
| `disconnected` | 连不上 ser2net | 检查 Dev Host IP/端口; `nc -zv host port` |
| `DUT-off` | 目标无输出 | `serial_send_command "echo ping"`; `serial_reset` |
| `crashed` | 内核崩溃 | 查看日志: `serial_get_logs(pattern="panic")` |
| `send_command` 返回空 | 目标 shell 未就绪 | 等待 `booted` 状态; 检查 login 是否完成 |
| 第二个 Claude Code 被拒 | 同 host:port 互斥 | 退出第一个实例或等它释放锁 |
| 高波特率丢数据 | 流控不匹配 | 降低波特率或启用 rfc2217 + RTSCTS |

## 依赖

```
fastmcp >= 3.0.0    # MCP server framework
pyserial >= 3.5      # serial.serial_for_url("socket://")
pexpect >= 4.9       # PtxExpect 继承
attrs >= 23.0        # 数据类装饰器
```

## 参考

- 设计文档: `docs/design/embedded-debug-mcp-v4.md`
- Review: `docs/design/embedded-debug-mcp-v4-review.md`
- labgrid: https://github.com/labgrid-project/labgrid
- LAVA: https://github.com/Linaro/lava
