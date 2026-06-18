# Embedded Debug Skill — Rust MCP v0.2

Rust MCP Server (mcp-rs/) + Python hooks. TCP 直连 Dev Host ser2net，零 socat，零 SSH。

## 架构

```
Claude Code                         Dev Host (192.168.1.xxx)
┌─────────────────────┐            ┌──────────────────────────┐
│  statusline hook     │            │  ser2net                  │
│  (inotify 事件驱动)   │            │  TCP:2000 → /dev/ttyACM0 │
│         │            │            │  TCP:2001 → 继电器       │
│  ┌──────▼──────────┐ │   TCP:2000 │                          │
│  │  Rust MCP Server │─┼──────────→│  目标板 (RK3576/LR3576)  │
│  │  (stdio/HTTP)    │ │   TCP:2001 │                          │
│  │                  │─┼──────────→│  ┌─RESET─┐ ┌─MASKROM─┐   │
│  │  • SerialEngine  │ │            │  │  ch1   │ │   ch2    │   │
│  │  • BootDetector  │ │            │  └───┬────┘ └────┬─────┘   │
│  │  • StageLearner  │ │            │      │          │         │
│  │  • RelayManager  │ │            │  目标板 RK3576  Android   │
│  │  • LogManager    │ │            └──────────────────────────┘
│  │  • StateManager  │─┼──▶ target-state ──inotify──▶ statusline
│  └──────────────────┘ │
└─────────────────────┘
```

## 快速开始

```bash
# 构建
cd mcp-rs && cargo build --release

# 配置
cp .target.conf.example .target.conf
vi .target.conf   # 设置 RK_DEV_HOST_IP, RK_SERIAL_PORT
```

Claude Code 启动时，SessionStart hook 自动检测 `.target.toml`（或 `.target.conf`）。在 **stdio 模式**下（默认），Claude Code 根据 `.mcp.json` 自动 spawn Rust MCP Server；在 **HTTP 模式**下，hook 启动 HTTP server 进程。

## 功能

| 功能 | 实现 |
|------|------|
| 串口交互 | MCP tools: `serial_send_command`, `serial_get_state`, `serial_wait_pattern` |
| 启动日志 | 按上电周期自动切割 (`boot-NNN_*.log`) |
| U-Boot 中断 | 检测 autoboot → Ctrl-C pre-flood → `=>` 提示符 |
| 自动登录 | 识别 login/password 提示 → 自动发送凭据 |
| 崩溃检测 | Kernel panic/BUG/Oops → `crashed` 状态 |
| 继电器控制 | 4 字节协议 over TCP → `serial_reset`, `serial_enter_uboot`, `serial_enter_maskrom` |
| 跨 SOC 自适应 | StageLearner: 参考日志 → 文本相似度 → 匹配新 SOC 启动阶段 |
| 状态栏 | inotify 事件驱动，即时更新 (非轮询) |

## MCP Tools

| Tool | 功能 |
|------|------|
| `serial_send_command` | 在目标上执行 shell 命令 |
| `serial_get_state` | 获取目标状态 (active/booting/uboot/crashed/DUT-off) |
| `serial_get_logs` | 检索串口日志 (支持正则过滤) |
| `serial_list_logs` | 列出所有启动日志 |
| `serial_reset` | 硬件复位 + 日志切割 |
| `serial_enter_uboot` | 强制进入 U-Boot 交互提示符 |
| `serial_enter_maskrom` | 强制进入 Rockchip MASKROM 模式 |
| `serial_wait_pattern` | 阻塞等待指定模式出现 |
| `serial_uboot_command` | 在 U-Boot 提示符下发送命令 |
| `serial_new_log` | 手动切割日志 |
| `serial_poll_logs` | 增量获取新输出 |
| `serial_get_config` | 查看当前配置 |
| `serial_claim` | 夺取串口所有权 |
| `serial_load_reference` | 加载参考日志 (自适应阶段检测) |
| `serial_get_stages` | 查看已学习阶段指纹 |

## 配置 (`.target.conf`)

```bash
# 串口连接 (必填)
DEV_HOST_IP=192.168.1.xxx
SERIAL_PORT=2000

# 继电器控制
RELAY_PORT=2001
RESET_CHANNEL=1
MASKROM_CHANNEL=2

# 登录凭据
LOGIN_USER=root
LOGIN_PASS=

# U-Boot 中断策略 (lava/aggressive)
UBOOT_INTERRUPT_STRATEGY=lava

# 监控
HANG_TIMEOUT=60
HANG_HYSTERESIS=3
MAX_ARCHIVED_LOGS=10

# StageLearner: 参考启动日志 (新 SOC 自适应)
REFERENCE_LOG=/path/to/reference-boot.log
```

## Transport 模式

| Mode | Config | 说明 |
|------|--------|------|
| **stdio** (默认) | `"type":"stdio"` | Claude Code spawn 子进程，低延迟 |
| **HTTP** (备用) | `--http [HOST:PORT]` | 独立进程，端口 3000 |

## Hook 集成

`~/.claude/hooks/embedded-debug/` 中所有 hook 为 Python 脚本。

| Hook | 脚本 | 触发时机 | 作用 |
|------|------|---------|------|
| SessionStart | `session-start.py` | 进入项目 | 启动 statusline daemon + MCP 配置 |
| Stop | `session-stop.py` | 退出会话 | 清理 |
| PreToolUse | `pre-tool-use.py` | Bash 执行前 | 拦截原始串口/继电器访问 → 提醒用 MCP |
| UserPromptSubmit | `user-prompt-submit.py` | 每次提示前 | 目标 DUT-off/crashed 告警 |
| statusLine | `statusline.py` | 1s 刷新 | inotify 事件驱动，即时更新 |

## StageLearner — 跨 SOC 自适应

无需为每个新 SOC 写正则表达式。提供参考启动日志 → 自动学习阶段指纹。

```bash
# .target.conf 中配置 (启动时自动加载)
RK_REFERENCE_LOG=/path/to/mt6893-boot.log

# 或运行时手动加载
serial_load_reference("/path/to/new-soc-boot.log")
serial_get_stages  # → ddr:35, spl:14, bl31:42, uboot:8, kernel:15, ...
```

算法: 3-gram Jaccard 相似度 + 顺序约束。详见 [mcp-rs/README.md](mcp-rs/README.md).

## 文件结构

```
~/.config/ai-dev/skills/embedded-debug/
├── README.md
├── SKILL.md                    # Claude Code Skill 定义
├── mcp-rs/                     # Rust MCP Server
│   ├── src/
│   │   ├── main.rs             # 入口 (stdio + HTTP dispatch)
│   │   ├── mcp.rs              # JSON-RPC 2.0
│   │   ├── mcp_http.rs         # Streamable HTTP (axum)
│   │   ├── serial_engine.rs    # 核心引擎
│   │   ├── console.rs          # TCP 串口驱动
│   │   ├── boot_detector.rs    # Regex + StageLearner 双模式
│   │   ├── state_manager.rs    # 状态管理 (滞后防抖)
│   │   ├── log_manager.rs      # 日志切割 + 保留策略
│   │   ├── command_queue.rs    # 命令队列 (marker echo)
│   │   ├── relay_manager.rs    # 继电器控制 (4-byte 协议)
│   │   ├── lock_manager.rs     # 互斥锁 (O_EXCL)
│   │   ├── config.rs           # shell 风格配置解析
│   │   └── marker.rs           # 标记符生成
│   └── .target.conf
└── mcp-python/                 # Python MCP (legacy)
    └── ...

~/.claude/hooks/embedded-debug/
├── lib.py                      # 共享工具
├── session-start.py            # 自动启动
├── session-stop.py
├── pre-tool-use.py             # 拦截原始串口访问
├── user-prompt-submit.py       # 状态告警
└── statusline.py               # inotify 事件驱动
```

## 状态栏

**事件驱动**，非轮询。MCP 状态变化 → `target-state` 文件更新 → inotify 检测 → 即时刷新。

```
● serial:active        — 目标就绪，可执行命令
◐ serial:booting       — 启动中 (SPL → kernel)
● serial:uboot         — U-Boot 交互模式
✗ serial:crashed       — 内核崩溃 (panic/BUG/Oops)
✗ serial:DUT-off       — 目标无响应
✗ serial:disconnected  — 连不上 dev host (ser2net 不可达)
```
