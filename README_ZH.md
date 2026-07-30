# Debug Console MCP Server

基于 MCP 协议的嵌入式 Linux 目标板串口调试工具。通过 Dev Host ser2net 连接目标板，
TCP 直连，支持启动日志捕获、自动登录、崩溃检测、U-Boot 中断、继电器复位、
跨 SOC 自适应阶段检测（StageLearner）。

[English](README.md)

## 快速开始

```bash
# 1. 安装 Claude Code 插件（自动安装 debug-console-mcp 二进制）
claude plugin marketplace add https://github.com/bitshelf/debug-console-mcp
claude plugin install embedded-debug@bitshelf/debug-console-mcp

# 2. 创建配置文件和 DUT 目录
cp references/.target.toml.example .target.toml
vi .target.toml                            # 设置 [[dev_hosts]].ip, [dut.serial].port, [dut].alias
mkdir -p .dut-serial/$(grep alias .target.toml | head -1 | cut -d'"' -f2)/logs

# 3. 重启 Claude Code → 插件自动启动 MCP
```

## 硬件设置 — Dev Host

### udev 持久化命名

防止 `/dev/ttyACM*` 在重插/重启后重新编号：

```bash
# 在 dev host 上创建 /etc/udev/rules.d/99-rk3576-duts.rules:
SUBSYSTEM=="tty", ATTRS{serial}=="<USB序列号>", ATTRS{bInterfaceNumber}=="00", \
  SYMLINK+="serial/by-alias/<dut别名>"

sudo udevadm control --reload-rules && sudo udevadm trigger
ls -la /dev/serial/by-alias/
```

### ser2net 配置

```yaml
# /etc/ser2net.yaml — 端口 → 串口映射
connection: &con2000
    accepter: tcp,0.0.0.0,2000
    enable: on
    options:
      kickolduser: true
      telnet-brk-on-sync: true
    connector: serialdev,
              /dev/serial/by-alias/<dut别名>,
              115200n81,local
```

## 配置参考 (`.target.toml`)

### 单板配置

```toml
[[dev_hosts]]
alias = "rk-board-pc"
ip = "192.168.1.105"
user = "linaro"

[[dut]]
alias = "rk3576-board1"
dev_host = "rk-board-pc"

[dut.serial]
port = 2000

[dut.target]
login_user = "root"

[dut.uboot]
interrupt_char = "ctrl_c"
interrupt_strategy = "aggressive"

[dut.relay]
type = "usb-relay"
port = 2001
reset_ch = 1
reset_time_ms = 3000

[dut.monitor]
hang_timeout = 60
max_archived_logs = 10
reference_log = ".dut-serial/rk3576-board1/reference-boot.log"

[dut.flash]
tool = "upgrade_tool"
upload_dir = "/tmp"
full_image_cmd = "uf {image}"
kernel_image_cmd = "di -k {image}"
```

### 多板配置

添加多个 `[[dut]]` 块，每个 DUT 独立目录、状态文件、日志和继电器配置：

```toml
[[dut]]
alias = "rk3576-board1"
# ... 配置 ...

[[dut]]
alias = "rk3576-board2"
dev_host = "rk-board-pc"

[dut.serial]
port = 2008    # 不同端口！

[dut.target]
login_user = "root"

[dut.monitor]
reference_log = ".dut-serial/rk3576-board2/reference-boot.log"
```

## MCP 工具 (28 个)

| 工具 | 说明 |
|------|------|
| `serial_send_command` | 在目标板上执行 Shell 命令 |
| `serial_get_state` | 获取状态 (active/booting/uboot/crashed/DUT-off/disconnected) |
| `serial_get_logs` | 获取串口日志（带语法高亮） |
| `serial_list_logs` | 列出已归档的启动日志 |
| `serial_reset` | 继电器硬件复位 + 日志轮转 |
| `serial_power_cycle` | 继电器断电重启 |
| `serial_enter_uboot` | 进入 U-Boot（Ctrl-C flood，兼容 bootdelay=0） |
| `serial_reboot_uboot` | 软重启 + Ctrl-C flood → U-Boot |
| `serial_enter_maskrom` | 进入 MASKROM 模式 |
| `serial_wait_pattern` | 等待正则模式匹配 |
| `serial_uboot_command` | 在 U-Boot `=>` 提示符下发送命令 |
| `serial_new_log` | 手动轮转日志 |
| `serial_poll_logs` | 增量获取日志 |
| `serial_get_config` | 查看当前配置 |
| `serial_get_metrics` | 引擎指标：运行时间、命令/错误计数 |
| `serial_claim` | 声明串口所有权 |
| `serial_button` | 控制按键 (press/release/pulse) |
| `serial_pause` | 暂停串口引擎供 dutabo 接管 |
| `serial_resume` | 恢复串口引擎 |
| `serial_send_raw` | 发送原始字节 |
| `serial_load_reference` | 加载参考日志用于 StageLearner |
| `serial_get_stages` | 查看已学习的阶段指纹 |
| `serial_get_unclassified` | 获取未分类行 |
| `serial_append_reference` | 追加锚点行 + 热重载 StageLearner |
| `serial_learn_connection` | 自动学习连接（3次复位+相似度检测） |
| `serial_verify_relay` | 验证 CH340 继电器控制 |
| `serial_flash_plan` | 生成烧录计划 |
| `serial_flash` | 执行固件烧录 |

## 目标板状态

```
● serial:active        — DUT 就绪，已配置登录
◐ serial:booting       — 启动中
● serial:uboot         — U-Boot 交互式提示符
✗ serial:crashed       — 检测到内核崩溃
✗ serial:DUT-off       — 无响应（挂起超时）
✗ serial:disconnected  — ser2net 不可达
```

多 DUT 时 statusline 显示所有状态：
```
● rk3576-board1:active  ● rk3576-board2:active
```

## StageLearner 调优

StageLearner 使用文本相似度（3-gram Jaccard + Jaro-Winkler）检测启动阶段，无需 SOC 特定的正则表达式。

### `learner_stage_threshold`（默认 0.45）

| 范围 | 效果 |
|------|------|
| 0.30–0.40 | 宽松 — 可能产生误检 |
| **0.45–0.55** | **平衡（推荐）** |
| 0.60–0.80 | 严格 — 可能漏掉有效阶段 |

### `learner_crash_threshold`（默认 0.50）

```toml
[dut.monitor]
learner_stage_threshold = 0.50
learner_crash_threshold = 0.40
```

## CLI (`dutabo`)

```bash
cargo install --git https://github.com/bitshelf/debug-console-mcp

dutabo list                     # 列出 DUT
dutabo serial -d <别名>          # 交互式串口控制台 (Ctrl-T q 退出)
dutabo state -d <别名>           # 查看状态
dutabo uboot -d <别名>           # 进入 U-Boot
dutabo uf <镜像> -d <别名>       # 烧录固件
```

## 已知限制

- **BusyBox ash 管道缓冲**: `echo ... | grep ...` 可能返回空，改为 `printf` 或加 `; true`
- **首个命令预热**: MCP 启动后第一条 `serial_send_command` 可能返回空，先发 `echo warmup`
- **USB 继电器最小复位时间**: CH340 默认 3000ms，可通过 `reset_time_ms` 覆盖

## #[tool] 宏迁移（待完成）

当前使用手动 `ServerHandler` 实现，待 rmcp 3.x API 稳定后迁移到 `#[tool_router]` + `#[tool_handler]` 模式。详见 [README.md](README.md) 中的迁移计划。

## 参考

- [adancurusul/serial-mcp-server](https://github.com/adancurusul/serial-mcp-server) — 参考实现
- [rmcp 文档](https://docs.rs/rmcp/latest/rmcp/)
- [设计文档](docs/tech-design.md)
