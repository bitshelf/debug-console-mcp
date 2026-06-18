# Embedded Debug MCP Server — Rust 实现

基于 MCP 协议的嵌入式 Linux 目标板串口调试工具。通过 Dev Host ser2net 连接目标板，
TCP 直连（无 socat），提供启动日志捕获、自动登录、崩溃检测、U-Boot 中断、
继电器复位、**跨 SOC 自适应阶段检测**等功能。

Rust edition 2024，零框架 MCP 实现（纯 JSON-RPC 2.0 over stdio）。

## 架构

```
Build Machine                          Dev Host
┌──────────────────────┐              ┌──────────────────────┐
│ Claude Code           │              │ ser2net               │
│  ┌─────────────────┐  │   TCP:2000  │ TCP:2000 → /dev/ttyUSB0│
│  │ MCP Server      ├──┼────────────→│ (目标板串口)           │
│  │ (stdio, 子进程)  │  │   TCP:2001  │ TCP:2001 → /dev/ttyUSB1│
│  │ tokio TCP 直连   │  ├────────────→│ (CH340 继电器)         │
│  └─────────────────┘  │              └──────────────────────┘
└──────────────────────┘
```

## 构建

```bash
cd ~/.config/ai-dev/skills/embedded-debug/mcp-rs
cargo build --release
# Binary: target/release/embedded-debug-mcp (3.0MB stripped)
```

## 配置

在项目根目录创建 `.target.conf`:

```bash
# 必填: 串口连接
DEV_HOST_IP=192.168.1.xxx
SERIAL_PORT=2000

# 可选: 自动登录
LOGIN_USER=root
LOGIN_PASS=mypassword

# 可选: 继电器复位
RELAY_PORT=2001
RESET_CHANNEL=2
MASKROM_CHANNEL=1
```

## MCP Tools

| Tool | 功能 |
|------|------|
| `serial_send_command` | 在目标上执行 shell 命令 (marker echo 模式) |
| `serial_get_state` | 获取目标状态和元数据 |
| `serial_get_logs` | 检索串口日志 (支持正则过滤) |
| `serial_list_logs` | 列出所有归档启动日志 |
| `serial_reset` | 硬件复位 + 日志切割 |
| `serial_enter_uboot` | 强制进入 U-Boot 交互提示符 (retry up to failure_retry times) |
| `serial_reboot_uboot` | 软重启 + Ctrl-C flood 进入 U-Boot (bootdelay=0 也有效) |
| `serial_enter_maskrom` | 强制进入 Rockchip MASKROM 模式 |
| `serial_wait_pattern` | 阻塞等待串口输出中出现指定模式 |
| `serial_uboot_command` | 在 U-Boot 提示符下发送命令 |
| `serial_new_log` | 手动切割日志 (不复位) |
| `serial_poll_logs` | 增量获取新输出 (文件位置跟踪) |
| `serial_get_config` | 获取当前目标配置 |
| `serial_claim` | 夺取串口所有权 |
| `serial_load_reference` | 加载参考启动日志启用自适应阶段检测 |
| `serial_get_stages` | 查看已学习的启动阶段指纹 |

## 模块结构

```
src/
├── main.rs              # 入口: 初始化日志 + 启动 engine + MCP server
├── mcp.rs               # MCP Server: JSON-RPC 2.0 over stdio + HTTP
├── mcp_http.rs          # Streamable HTTP transport (axum, :3000)
├── serial_engine.rs     # 核心引擎: 协调所有子系统
├── console.rs           # TCP 串口驱动: tokio TcpStream → ser2net
├── boot_detector.rs     # 启动阶段检测: regex + StageLearner 自适应
├── state_manager.rs     # 状态管理: 滞后防抖 + 原子写状态文件
├── log_manager.rs       # 日志管理: per-boot-cycle 切割 + 保留策略
├── command_queue.rs     # 命令队列: marker echo 串行化 + 响应路由
├── relay_manager.rs     # 继电器控制: 4 字节协议 over TCP
├── lock_manager.rs      # 互斥锁: O_EXCL 原子创建 + 僵尸清理
├── config.rs            # 配置解析: shell 风格 .target.conf
└── marker.rs            # 标记符生成: 10 字符随机大写字母
```

## StageLearner — 跨 SOC 自适应阶段检测

`boot_detector.rs` 支持双模式启动阶段检测：

### Mode 1: Regex 精确匹配 (默认)

预编译正则表达式检测已知 SOC (RK3576, RK3588 等) 的启动阶段。
零额外开销，适合日常使用。

### Mode 2: StageLearner 自适应 (新 SOC)

当调试新的/未知 SOC 时，无需修改代码添加正则表达式。
只需提供一份完整的参考启动日志：

```bash
# 1. 在 Claude Code 中加载参考日志
serial_load_reference("/path/to/new-soc-boot.log")

# 2. 查看已学习的阶段指纹
serial_get_stages
# → ddr: 35, spl: 14, bl31: 42, optee: 91, uboot: 8, kernel: 15, ...

# 3. 后续所有串口输出自动使用自适应检测
# StageLearner 使用 3-gram Jaccard 相似度 + 顺序约束
```

### 算法

```
对于每一行串口输出:
  1. 计算与所有参考指纹的 3-gram Jaccard 相似度
  2. 选择得分最高的阶段 (及其上下文锚定行)
  3. 阈值过滤 (< 0.45 不匹配)
  4. 顺序约束 (不允许阶段倒退 > 1)
  5. 崩溃检测不受顺序约束 (任意位置均可触发)
```

### 跨 SOC 验证

| SOC | 检测阶段 | 结果 |
|-----|---------|------|
| RK3576 (参考) | 9/9 | ✅ |
| MediaTek MT6893 (模拟) | 9/9 | ✅ |
| Qualcomm 骁龙 (模拟) | 8/9 | ⚠️ 缺 OP-TEE |

## 依赖

| Crate | 版本 | 用途 |
|-------|------|------|
| `tokio` | 1 | async runtime, TCP, sync primitives |
| `serde` | 1 | 序列化 |
| `serde_json` | 1 | JSON-RPC 编解码 |
| `regex` | 1 | boot stage / crash pattern / ANSI 清理 |
| `toml` | 0.8 | TOML 配置解析 (.target.toml) |
| `md-5` | 0.10 | 项目路径哈希 (statusline /dev/shm key) |
| `inotify` | 0.11 | (removed: statusline-watch daemon deleted) |
| `rand` | 0.8 | marker 生成 |
| `chrono` | 0.4 | 时间戳格式化 |
| `tracing` | 0.1 | 结构化日志 |
| `tracing-subscriber` | 0.3 | 日志格式化 + env-filter |
| `once_cell` | 1 | static lazy init (regex cache) |
| `strsim` | 0.11 | StageLearner 文本相似度 (Sorensen-Dice) |
| `axum` | 0.8 | Streamable HTTP transport |
| `tower-http` | 0.6 | CORS middleware (HTTP transport) |

## 与 Python 版本的差异

| 特性 | Python | Rust |
|------|--------|------|
| MCP 框架 | 零框架 (手写 JSON-RPC) | 零框架 (手写 JSON-RPC) |
| 串口连接 | pyserial `socket://` | tokio `TcpStream` |
| 并发模型 | asyncio + thread pool | tokio (native async) |
| expect 能力 | pexpect 继承 (PtxExpect) | boot detector 事件模型 |
| 二进制大小 | ~50MB (Python + venv) | 3.0MB (stripped) |
| 启动时间 | ~500ms | ~10ms |
| 内存占用 | ~30MB | ~5MB |

## 日志

运行时日志写入 `{project}/.dut-serial/mcp.log`。
串口数据日志写入 `{project}/.dut-serial/logs/boot-NNN_*.log`。

## 命令行

```bash
# 查看帮助
embedded-debug-mcp --help
embedded-debug-mcp -h

# 查看版本
embedded-debug-mcp --version

# 启动 (默认: info 日志到文件)
embedded-debug-mcp

# 调试模式 (debug 日志)
embedded-debug-mcp --verbose

# 日志输出到 stderr (调试时方便查看)
embedded-debug-mcp --log-to-stderr -v
```

## 部署到 Claude Code

### 方式一: 手动配置 `.mcp.json` (推荐)

在项目根目录创建 `.mcp.json`:

```json
{
  "mcpServers": {
    "embedded-debug": {
      "command": "$HOME/.claude/skills/embedded-debug/mcp-rs/target/release/embedded-debug-mcp",
      "args": []
    }
  }
}
```

### 方式二: SessionStart Hook 自动生成

如果项目已有 `.target.conf`，`session-start.py` hook
会自动检测并生成 `.mcp.json`:

```bash
# Hook 会自动生成的 .mcp.json 示例:
{
  "mcpServers": {
    "embedded-debug": {
      "command": "/home/user/.claude/skills/embedded-debug/mcp-rs/target/release/embedded-debug-mcp",
      "args": []
    }
  }
}
```

### 方式三: 全局安装

```bash
# 编译 release 二进制
cd ~/.config/ai-dev/skills/embedded-debug/mcp-rs
cargo build --release

# 安装到 PATH
sudo cp target/release/embedded-debug-mcp /usr/local/bin/

# 然后在 .mcp.json 中使用:
{
  "mcpServers": {
    "embedded-debug": {
      "command": "embedded-debug-mcp",
      "args": []
    }
  }
}
```

### 验证部署

```bash
# 1. 确认二进制可执行
file target/release/embedded-debug-mcp
# 应该输出: ELF 64-bit ... executable ...

# 2. 确认帮助输出
target/release/embedded-debug-mcp --help

# 3. 确认项目有 .target.conf
ls -la .target.conf

# 4. 启动 Claude Code 后查看状态栏
# 成功时会显示: ● serial:active 或 ● serial:disconnected
```

### 工作流

```
1. 构建         cargo build --release
2. 配置         vi .target.conf          (在项目根目录)
3. 部署         vi .mcp.json             (或依赖 hook)
4. 启动         claude                   (Claude Code 自动 spawn MCP Server)
5. 使用         serial_send_command "uname -a"
6. 查看日志     tail -f .dut-serial/mcp.log
```

### 故障排除

| 现象 | 原因 | 解决 |
|------|------|------|
| 状态栏不显示 | 无 `.target.conf` 或 Server 未启动 | 检查项目根目录; 运行 `--log-to-stderr -v` 调试 |
| `disconnected` | 连不上 ser2net | 检查 IP/端口; `nc -zv host port` |
| `DUT-off` | 目标无输出 | `serial_send_command "echo ping"`; `serial_reset` |
| 二进制不启动 | 依赖库缺失 | 检查动态链接: `ldd target/release/embedded-debug-mcp` |
| 第二个实例被拒 | 同 host:port 互斥 | 退出第一个实例或等它释放锁 |

### 测试

```bash
# 运行全部单元测试
cargo test

# 运行特定模块测试
cargo test boot_detector
cargo test command_queue
cargo test mcp

# 查看测试覆盖率报告
cargo test -- --nocapture
```
