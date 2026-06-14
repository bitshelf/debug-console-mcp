# embedded-debug-mcp 部署指南

## 概述

`embedded-debug-mcp` 是一个标准的 **MCP (Model Context Protocol) 服务器**，
使用 stdio transport (JSON-RPC 2.0 newline-delimited)。

**不依赖任何特定 Agent** — 只要是 MCP 兼容的客户端都能使用。

## 快速开始 (2 步)

### 第一步: 构建 & 安装

```bash
cd embedded-debug/mcp-rs

# 方式 A: 一键部署
./deploy.sh

# 方式 B: 手动
cargo build --release
cp target/release/embedded-debug-mcp ~/.local/bin/
```

### 第二步: 配置 `.target.conf`

在 SDK 项目根目录创建:

```bash
# 必填: ser2net 连接
RK_DEV_HOST_IP=192.168.1.231
RK_SERIAL_PORT=2000

# 可选: 自动登录
RK_LOGIN_USER=root
RK_LOGIN_PASS=

# 可选: 继电器复位
RK_RELAY_PORT=2001
RK_RESET_CHANNEL=1
```

### 第三步: 选择 Agent 配置

#### Claude Code

`.mcp.json` (项目根目录):
```json
{
  "mcpServers": {
    "embedded-debug": {
      "command": "embedded-debug-mcp",
      "env": { "RUST_LOG": "info" }
    }
  }
}
```

#### Continue (VS Code / JetBrains)

`~/.continue/config.json`:
```json
{
  "experimental": {
    "mcpServers": [
      {
        "name": "embedded-debug",
        "command": "embedded-debug-mcp",
        "cwd": "/path/to/your/sdk"
      }
    ]
  }
}
```

#### Cursor

`.cursor/mcp.json` (或全局 `~/.cursor/mcp.json`):
```json
{
  "mcpServers": {
    "embedded-debug": {
      "command": "embedded-debug-mcp",
      "env": { "RUST_LOG": "info" }
    }
  }
}
```

#### Codex (OpenAI)

`codex.yaml`:
```yaml
mcp_servers:
  - name: embedded-debug
    command: embedded-debug-mcp
    env:
      TARGET_CONF: /path/to/.target.conf
```

#### Zed

`.zed/settings.json`:
```json
{
  "mcp": {
    "embedded-debug": {
      "command": {
        "path": "embedded-debug-mcp",
        "env": {
          "TARGET_CONF": "/path/to/.target.conf"
        }
      }
    }
  }
}
```

#### Goose (Block)

`~/.config/goose/mcp.toml`:
```toml
[mcp_servers.embedded-debug]
command = "embedded-debug-mcp"
cwd = "/path/to/sdk"
```

#### 通用 (手动启动)

```bash
# 直接启动 (bash / zsh)
TARGET_CONF=/path/to/.target.conf RUST_LOG=info embedded-debug-mcp

# stdin/stdout 就是 JSON-RPC:
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | embedded-debug-mcp
```

### 协议验证

```bash
# 测试 initialize 握手
printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}\n' | \
  TARGET_CONF=/path/to/.target.conf embedded-debug-mcp 2>/dev/null

# 预期输出:
# {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05",...}}

# 列出所有 tools
printf '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}\n' | \
  TARGET_CONF=/path/to/.target.conf embedded-debug-mcp 2>/dev/null
```

## 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `TARGET_CONF` | `.target.conf` 路径 | 从 CWD 向上搜索 |
| `RUST_LOG` | 日志级别 (error/warn/info/debug/trace) | info |

## 工作原理

```
┌──────────────────────────────────────────────────────┐
│ 任何 MCP 客户端 (Claude/Cursor/Continue/Zed/...)      │
│                                                      │
│  1. spawn embedded-debug-mcp 子进程                   │
│  2. stdin → JSON-RPC request                          │
│  3. stdout → JSON-RPC response                        │
└──────────┬───────────────────────────────────────────┘
           │ stdio (pipe)
┌──────────▼───────────────────────────────────────────┐
│ embedded-debug-mcp (Rust binary, 3MB)                 │
│                                                      │
│  mcp.rs → handle_message()                           │
│    ├── initialize → protocol version 2024-11-05      │
│    ├── tools/list → 10 serial_* tools                │
│    └── tools/call → SerialEngine                     │
│         ├── console.rs → TCP → ser2net :2000         │
│         ├── relay.rs   → TCP → ser2net :2001         │
│         ├── boot_detector.rs → 模式匹配              │
│         ├── command_queue.rs → marker echo           │
│         └── log_manager.rs → .dut-serial/logs/       │
└──────────────────────────────────────────────────────┘
```

## 15 个 MCP Tools

| Tool | 说明 |
|------|------|
| `serial_send_command` | 执行 shell 命令 (marker echo 模式) |
| `serial_get_state` | 获取目标状态 (active/booting/uboot/crashed/...) |
| `serial_get_logs` | 检索串口日志 (支持行数限制 + 正则过滤) |
| `serial_list_logs` | 列出所有归档启动日志 |
| `serial_reset` | 继电器硬件复位 + 日志切割 |
| `serial_enter_uboot` | 强制进入 U-Boot 交互提示符 |
| `serial_enter_maskrom` | 强制进入 Rockchip MASKROM 模式 |
| `serial_wait_pattern` | 阻塞等待指定正则模式出现 |
| `serial_uboot_command` | 在 U-Boot 提示符下发送命令 |
| `serial_new_log` | 手动切割日志 (不复位) |
| `serial_poll_logs` | 增量获取新输出 |
| `serial_get_config` | 获取当前目标配置 |
| `serial_claim` | 夺取串口所有权 |
| `serial_load_reference` | 加载参考日志启用自适应阶段检测 |
| `serial_get_stages` | 查看已学习的启动阶段指纹 |

## 故障排除

```bash
# 查看帮助
embedded-debug-mcp --help

# 调试模式 (日志输出到 stderr)
embedded-debug-mcp --log-to-stderr -v

# 手动测试连接
nc -zv 192.168.1.231 2000

# 模拟 MCP 客户端
TARGET_CONF=.target.conf embedded-debug-mcp --log-to-stderr -v
```
