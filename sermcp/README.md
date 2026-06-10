# Embedded Debug MCP Server — Rust 实现

基于 MCP 协议的嵌入式 Linux 目标板串口调试工具。通过 Dev Host ser2net 连接目标板，
TCP 直连（无 socat），提供启动日志捕获、自动登录、崩溃检测、U-Boot 中断、
继电器复位、**跨 SOC 自适应阶段检测**等功能。

Rust edition 2024，零框架 MCP 实现，支持 stdio JSON-RPC 2.0 和 HTTP transport。

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
cd sermcp
cargo build --release --locked
# Binary: target/release/sermcp
```

## 配置

在项目根目录创建 `.target.jsonc`（JSONC：带 `//` 注释的 JSON），用
`dutabo init` 交互式创建/更新（保留注释）。`.mcp.json` 缺失或没有
`sermcp` 条目时，`dutabo init` 会自动创建/补齐该条目（已有条目则
原样保留）。

`dutabo init --tui` 的表单是网格布局：每个 dev host 一行（左侧主机字段、
右侧相邻列的 DUT 子块，行间用横线分隔）；`+`/`x` 按钮（键盘 Tab + Enter
或鼠标点击均可）添加/删除主机与 DUT 行，数字计数字段已被按钮取代（顶部
只保留实时计数）。每个 DUT 的 relay 区默认收起——「use relay?」开关默认
NO，选 YES 才展开类型/端口/通道/复位时长等子字段（更新时按现有 relay
区自动预填）。主机数为 0 时整个 DUT 区消失，写入的配置为
`"dev_hosts": []`（0 主机 = 0 DUT）。每 DUT 的 section 组（serial/target/
uboot/relay/monitor/flash/control）在宽度 ≥ 100 列时按左右两栏紧凑排布，
低于该阈值退化为单列。

**.target.jsonc:**

```jsonc
{
  "dev_hosts": [
    {
      "ip": "192.168.1.xxx",
      "user": "linaro",
      "duts": [
        {
          "dut_name": "rk3576",
          "serial": { "port": 2000 },
          "target": {
            "login_user": "root",
            "login_prompt": ""
          },
          "uboot": {
            "interrupt_char": "ctrl_c",
            "interrupt_strategy": "flood"
          },
          "relay": {
            "type": "ch340-relay"          // ch340 | ch340-relay | none | disabled
            // "port": 2001,
            // "reset_ch": 1,
            // "maskrom_ch": 2,
            // "power_ch": 4
          },
          "monitor": {
            "hang_timeout": 60,
            "max_archived_logs": 10,
            "reference_log": ".dut-serial/rk3576/reference-boot.log"
          }
        }
      ]
    }
  ]
}
```

完整带注释 schema：`references/.target.jsonc.example`。

`relay.type` 在运行时通过 `dut-ctrl` 的 **plugin 注册表**选择 backend：
`ch340` / `ch340-relay` 使用 `dut-ctrl/src/ch340.rs`；`none`/`disabled`
新增后端只需实现 `PowerControl` trait 并调用 `register_backend(...)` 注册，
引擎与 MCP 层零修改。CH340 是默认 Cargo feature，可用
`cargo build --no-default-features` 构建无任何后端（disabled-only）的版本。

## MCP Tools

| Tool | 功能 |
|------|------|
| `serial_send_command` | 在目标上执行 shell 命令 (marker echo 模式) |
| `serial_get_state` | 获取目标状态和元数据 |
| `serial_get_logs` | 检索串口日志 (支持正则过滤) |
| `serial_list_logs` | 列出所有归档启动日志 |
| `serial_reset` | 硬件复位 + 日志切割；`wait_boot=true` 对 Tasks 客户端返回可轮询任务句柄 |
| `serial_power_cycle` | 通过电源通道断电再上电 |
| `serial_enter_uboot` | 通过 auto/hardware/software/none 重启策略进入 U-Boot 提示符 |
| `serial_wait_pattern` | 阻塞等待串口输出中出现指定模式 |
| `serial_uboot_command` | 在 U-Boot 提示符下发送命令 |
| `serial_poll_logs` | 增量获取新输出 (文件位置跟踪) |
| `serial_get_config` | 获取当前目标配置 |
| `serial_get_metrics` | 获取引擎运行指标 |
| `serial_claim` | 夺取串口所有权 |
| `serial_button` | 控制 reset/recovery/maskrom 按钮 |
| `serial_send_raw` | 发送原始串口字节 |
| `serial_load_reference` | 加载参考启动日志并返回已学习阶段摘要 |
| `serial_append_reference` | 追加锚点行到参考日志 + 热重载 StageLearner |
| `serial_learn_connection` | 多次复位学习连接稳定性和参考日志 |
| `serial_verify_relay` | 读回验证 CH340 继电器 |
| `serial_flash_plan` | 根据 flash 配置生成烧录计划 |

## 模块结构

```
src/
├── main.rs              # 入口: 初始化日志 + 启动 engine + MCP server
├── mcp.rs               # MCP Server: JSON-RPC 2.0 over stdio
├── mcp_http.rs          # Streamable HTTP transport (axum, :3000)
├── serial_engine.rs     # 核心引擎: 协调所有子系统
├── console.rs           # TCP 串口驱动: tokio TcpStream → ser2net
├── boot_detector.rs     # 启动阶段检测: regex + StageLearner 自适应
├── state_manager.rs     # 状态管理: 滞后防抖 + 原子写状态文件
├── log_manager.rs       # 日志管理: per-boot-cycle 切割 + 保留策略
├── command_queue.rs     # 命令队列: marker echo 串行化 + 响应路由
├── dut-ctrl/            # 电源控制 plugin 注册表；relay.type 选择已注册后端
│   └── src/ch340.rs     # 可选 CH340 四字节协议实现（注册为 ch340/ch340-relay）
├── lock_manager.rs      # 互斥锁: O_EXCL 原子创建 + 僵尸清理
├── config.rs            # 配置 facade: .target.jsonc 发现 + 校验
├── config_jsonc.rs      # JSONC 解析: dev_hosts[].duts[] 嵌套 schema（严格 ParseOptions）
├── dut_state.rs         # .dut-serial/<dut_name>/target-state 读取 + 状态分类
├── inventory.rs         # .dut-serial/inventory.json（Python hooks 唯一数据源）
├── init.rs              # dutabo init 向导核心（headless，可测试）
├── flash.rs             # dev host 烧录计划和执行
├── inotify_watcher.rs   # DUT target-state 变更监听辅助（notify crate）
├── connection_learner.rs # 连接学习和参考日志生成
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
# 在 Claude Code 中加载参考日志
serial_load_reference("/path/to/new-soc-boot.log")

# 加载结果直接返回 fingerprint 总数和各阶段数量
# → {"fingerprints":42,"stages":{"ddr":5,"spl":4,"uboot":8,...}}

# 后续所有串口输出自动使用自适应检测
# StageLearner 使用 3-gram Jaccard 相似度 + 顺序约束
```

### 检测流程 (v0.2.0+)

```
对于每一行串口输出:
  1. 崩溃检测 (regex, 始终执行)
  2. StageLearner 优先 (如果已加载参考日志):
     a. 组合分数: 3-gram Jaccard * 0.6 + Jaro-Winkler * 0.4
     b. 阈值过滤 (默认 0.45, 低于此值不匹配)
     c. 顺序约束 (不允许阶段倒退 > 1)
     d. DDR/SPL 匹配 → 触发 BootStart (日志分割)
     e. 未匹配行 → 收集到 unclassified.log
  3. login/password regex (始终执行, 触发自动登录)
  4. Regex 回退 (仅当 StageLearner 未匹配时)
```

### Auto-Learning 闭环

```
StageLearner 未分类行
  → 读取 MCP Resource log://serial/unclassified
  → Agent 分析 + 裁剪关键锚点行
  → serial_append_reference (追加 + 热重载)
  → 下次 boot cycle 使用新指纹
  → StageLearner 越用越准
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
| `jsonc-parser` | 0.33 | JSONC 配置解析 (.target.jsonc + .mcp.json，仅 serde feature) |
| `toml` | 0.8 | 仅 build-dependencies（build.rs 读 Cargo.toml `[package.metadata.learn]`） |
| `ratatui` | 0.30 | `dutabo init` TUI（仅 CLI 二进制；lib 向导核心零 TUI） |
| `crossterm` | 0.29 | `dutabo init` TUI 传输层（raw mode / 事件 / alt screen） |
| `md-5` | 0.10 | 项目路径哈希 (statusline /dev/shm key) |
| `notify` | 8 | 跨平台配置/日志/状态文件事件监听（Linux 使用 inotify 后端） |
| `rand` | 0.9 | marker 生成 |
| `chrono` | 0.4 | 时间戳格式化 |
| `tracing` | 0.1 | 结构化日志 |
| `tracing-subscriber` | 0.3 | 日志格式化 + env-filter |
| `once_cell` | 1 | static lazy init (regex cache) |
| `strsim` | 0.11 | StageLearner: Jaccard + Jaro-Winkler + Sorensen-Dice |
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
sermcp --help
sermcp -h

# 查看版本
sermcp --version

# 启动 (默认: info 日志到文件)
sermcp

# 调试模式 (debug 日志)
sermcp --verbose

# 日志输出到 stderr (调试时方便查看)
sermcp --log-to-stderr -v
```

## 部署到 Claude Code

### 方式一: 手动配置 `.mcp.json` (推荐)

在项目根目录创建 `.mcp.json`:

```json
{
  "mcpServers": {
    "sermcp": {
      "command": "$HOME/.claude/skills/embedded-debug/sermcp/target/release/sermcp",
      "args": [],
      "env": {
        "TARGET_CONF": "/absolute/path/to/.target.jsonc"
      }
    }
  }
}
```

`TARGET_CONF` 可省略（CWD 位于项目内时自动向上查找 `.target.jsonc`），
显式指定时指向 `.target.jsonc`。

### 方式二: SessionStart Hook 自动生成

如果项目已有 `.target.jsonc`，`session-start.py` hook 会先运行一次
`dutabo list --json` 引导（别名/端口/dev host 全部来自 Rust），再为每个 DUT
启动 HTTP MCP。必要时也可以由代码生成 `.mcp.json`:

```bash
# Hook 会自动生成的 .mcp.json 示例:
{
  "mcpServers": {
    "sermcp": {
      "command": "/home/user/.claude/skills/embedded-debug/sermcp/target/release/sermcp",
      "args": ["--http"],
      "env": {
        "TARGET_CONF": "/home/user/project/.target.jsonc"
      }
    }
  }
}
```

### 方式三: 全局安装

```bash
# 编译 release 二进制
cd ~/.config/ai-dev/skills/embedded-debug/sermcp
cargo build --release

# 安装到 PATH
sudo cp target/release/sermcp /usr/local/bin/

# 然后在 .mcp.json 中使用:
{
  "mcpServers": {
    "sermcp": {
      "command": "sermcp",
      "args": []
    }
  }
}
```

### 验证部署

```bash
# 确认二进制可执行
file target/release/sermcp
# 应该输出: ELF 64-bit ... executable ...

# 确认帮助输出
target/release/sermcp --help

# 确认项目有 .target.jsonc
ls -la .target.jsonc

# 启动 Claude Code 后查看状态栏
# 成功时会显示: ● serial:active 或 ● serial:disconnected
```

### 工作流

```
1. 构建         cargo build --release
2. 配置         dutabo init              (在项目根目录；或 vi .target.jsonc)
3. 部署         vi .mcp.json             (或依赖 hook)
4. 启动         claude                   (Claude Code 自动 spawn MCP Server)
5. 使用         serial_send_command "uname -a"
6. 查看日志     tail -f .dut-serial/mcp.log
```

### 故障排除

| 现象 | 原因 | 解决 |
|------|------|------|
| 状态栏不显示 | 无 `.target.jsonc` 或 Server 未启动 | 检查项目根目录; 运行 `--log-to-stderr -v` 调试 |
| `disconnected` | 连不上 ser2net | 检查 IP/端口; `nc -zv host port` |
| `DUT-off` | 目标无输出 | `serial_send_command "echo ping"`; `serial_reset` |
| 二进制不启动 | 依赖库缺失 | 检查动态链接: `ldd target/release/sermcp` |
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
