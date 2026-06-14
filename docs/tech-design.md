# Serial Debug MCP 技术设计

> 日期：2026-06-22
> 状态：实施设计稿
> 范围：连接学习、日志分割、目标板控制抽象、多 DUT、服务器 CLI、烧录抽象、Agent 集成约束。

## 1. 目标

本设计将 `debug-console-mcp` 扩展为一套可被 Agent 和人工 CLI 同时使用的 DUT 控制与调试系统：

- 自动学习启动日志分割参考，判断串口连接是否真实建立。
- 将 reset、recovery、maskrom 抽象为“按下/松开”的按钮控制，不绑定 CH340。
- 支持 CH340 继电器自检，不可用时自动退化到软件 `reboot`。
- 支持多台 DUT 同时在线，通过别名选择。
- 提供服务器侧 `dutabo` 风格命令：串口、reset、状态、烧录。
- 烧录动作通过 `.target.toml` 中的命令模板抽象，支持不同 SoC。
- Agent 只能读取 `.target.toml`，不得生成或修改该文件。

## 2. 总体架构

```text
Agent / Claude Code                 Developer CLI
MCP tools                           dutabo
      |                                |
      +---------------+----------------+
                      |
              Serial Debug Engine
        +-------------+--------------+
        | config registry            |
        | serial engine              |
        | log learner / splitter     |
        | state manager              |
        | power-control abstraction  |
        | flash abstraction          |
        +-------------+--------------+
                      |
             Dev Host / ser2net / ssh
        +-------------+--------------+
        | UART TCP port              |
        | relay TCP port             |
        | upgrade_tool/rkdeveloptool |
        +-------------+--------------+
                      |
                     DUT
```

关键原则：

- 串口连接由 MCP engine 长期持有；CLI 查看实时日志时通过 MCP/HTTP 或日志文件订阅，不挤掉 Agent。
- 硬件控制能力是可选能力；缺失时明确降级，而不是假定存在。
- `.target.toml` 是唯一人工配置入口；Agent 只读。
- 日志分割基于学习得到的参考文本，正则仅用于崩溃、登录提示、U-Boot 提示符等强语义事件。

## 3. 配置设计

### 3.1 配置发现

从当前目录向上查找：

1. `.target.toml`
2. `.target.conf` 兼容旧格式

如果存在 `.target.toml` 但不存在 `.mcp.json`，SessionStart hook 自动生成 `.mcp.json` 并启动 MCP。

### 3.2 只读约束

Agent 禁止生成、修改 `.target.toml`。允许操作：

- 读取配置。
- 生成 `.mcp.json`。
- 生成 `.dut-serial/*` 下的运行时文件。
- 生成学习日志和 `reference_log` 指向的参考日志文件。

### 3.3 单 DUT 示例

```toml
# Embedded Debug Target Configuration
# Agent must not edit this file.

[dev_host]
ip = "192.168.1.105"
user = "linaro"
pass = ""

[serial]
ip = ""          # empty means dev_host.ip
port = 2000

[relay]
ip = ""          # empty means dev_host.ip
port = 2001
reset_ch = 1
# maskrom_ch = 2
# recovery_ch = 3

[target]
login_user = "root"
login_pass = ""

[monitor]
hang_timeout = 60
max_archived_logs = 10
reference_log = ".dut-serial/reference-boot.log"

[flash]
tool = "upgrade_tool"
upload_dir = "/tmp"
full_image_cmd = "uf {image}"
kernel_image_cmd = "di -k {image}"
loader_bin = "/opt/rockchip/rk3576_loader.bin"
loader_cmd = "db {loader}"
```

兼容字段：

- `reset_ch` 与 `reset_channel` 等价。
- `maskrom_ch` 与 `maskrom_channel` 等价。
- `recovery_ch` 与 `recovery_channel` 等价。
- `monitor.reference_log` 与顶层 `reference_log` 等价。

### 3.4 多 DUT 示例

```toml
[dev_host]
ip = "192.168.1.105"
user = "linaro"
pass = ""

[[dut]]
alias = "rk3576-a"

[dut.serial]
port = 2000

[dut.relay]
port = 2001
reset_ch = 1
maskrom_ch = 2

[dut.monitor]
reference_log = ".dut-serial/rk3576-a/reference-boot.log"

[[dut]]
alias = "rk3588-b"

[dut.serial]
port = 2010

[dut.relay]
port = 2011
reset_ch = 1

[dut.monitor]
reference_log = ".dut-serial/rk3588-b/reference-boot.log"
```

选择规则：

- 只有一台 DUT：默认选中。
- 多台 DUT：CLI 需要 `--dut <alias>`，未指定时交互选择。
- MCP 实例默认绑定当前项目的默认 DUT；后续可扩展工具参数 `dut_alias`。

## 4. 连接学习流程

### 4.1 硬件 reset 学习

流程：

1. 按下 reset。
2. 创建学习日志文件：`.dut-serial/learn/learn-<timestamp>-<n>.log`。
3. 松开 reset。
4. 捕获启动日志并写入文件。
5. 重复三次。
6. 比较三个文件前 50 行文本相似度。
7. 如果三者两两相似度均大于 93%，保留最后一次文本为 `reference_log`。
8. 输出日志分割参考路径，判定连接建立。

相似度：

- 读取每个文件前 50 行。
- 归一化：去 ANSI、去 NUL、去时间戳、压缩空白。
- 使用 `strsim::jaro_winkler` 与 n-gram Jaccard 加权：
  `score = jaro_winkler * 0.6 + jaccard * 0.4`。
- 三个文件的最低两两分数作为本轮分数。

### 4.2 继电器不可用判定

CH340 继电器控制流程：

1. 发出控制指令。
2. 读取继电器状态。
3. 读回值与发出值一致，认为继电器已连接且可控。

学习阶段额外判定：

- 如果三次硬件 reset 后的前 50 行相似度低于 10%，判定 reset 继电器不可用。
- 不继续依赖 relay reset。
- 切换为软件 `reboot` 学习。

### 4.3 软件 reboot 学习

适用场景：

- 未配置 reset 继电器。
- reset 继电器自检失败。
- reset 学习相似度低于 10%。

流程：

1. 发送 `reboot`。
2. 创建学习日志文件，第一行写入固定字符串 `reboot`。
3. 捕获重启日志并写入文件。
4. 重复三次。
5. 比较三个文件内容相似度。
6. 相似度大于 93%，判定连接建立。

如果没有 reset 继电器但已有 `reference_log`：

1. 执行 `reboot`。
2. 保存日志到带时间戳的文件，第一行为 `reboot`。
3. 与已有参考日志比较。
4. 三次结果相似度大于 93%，判定连接建立。

## 5. 日志分割设计

运行时日志分为三类：

- `full.serial.log`：连续全量日志，不截断。
- `current.serial.log`：当前 boot cycle。
- `boot-<n>_<timestamp>.log`：归档日志。

分割触发：

- 学习参考匹配到启动开头。
- DDR/SPL/U-Boot 早期启动锚点出现。
- 显式 `serial_new_log`。
- reset/reboot 控制动作开始。

分割策略：

- 检测到新启动周期时先 flush 当前 ring buffer。
- 新周期开始时清空 `current.serial.log`。
- 同一个 boot cycle 内使用 detector 去重，避免 DDR/SPL 多行重复切分。

参考日志：

- 学习成功后复制最后一次学习日志到 `reference_log`。
- MCP 启动时加载 `reference_log`。
- `serial_append_reference` 可追加锚点并热加载。

## 6. 电源与按键控制抽象

### 6.1 抽象接口

```rust
enum Button {
    Reset,
    Recovery,
    Maskrom,
}

trait PowerControl {
    async fn press(&mut self, button: Button) -> Result<()>;
    async fn release(&mut self, button: Button) -> Result<()>;
    async fn pulse(&mut self, button: Button, delay_ms: u64) -> Result<()>;
    async fn verify(&mut self) -> Result<VerifyResult>;
    fn has_button(&self, button: Button) -> bool;
}
```

reset、recovery、maskrom 统一表达为布尔状态：

- `true`：按下。
- `false`：松开。

### 6.2 后端

| 后端 | 当前状态 | 用途 |
| --- | --- | --- |
| CH340 relay | 实现 | reset/maskrom/recovery 按键模拟 |
| software reboot | 实现 | 无 relay 时软件重启 |
| SNMP v3 PDU | 预留 | 未来电源控制 |
| GPIO/button simulator | 预留 | 本机 GPIO 或外部控制程序 |

### 6.3 独立控制程序

控制程序编译为独立可执行程序，并在 `.target.toml` 配置：

```toml
[control]
dev_ctl = "embedded-dev-ctl"
```

如果 `dev_ctl` 未定义，则只使用内建后端。外部程序协议建议：

```bash
embedded-dev-ctl --dut rk3576-a button reset --pressed true
embedded-dev-ctl --dut rk3576-a button reset --pressed false
embedded-dev-ctl --dut rk3576-a verify
```

## 7. 状态机与 Agent 通知

状态集合：

- `active`：目标可交互。
- `booting`：正在启动。
- `uboot`：处于 U-Boot 交互提示符。
- `crashed`：检测到 Kernel panic/BUG/Oops。
- `DUT-off`：目标无输出或心跳失败。
- `disconnected`：串口 TCP/ser2net 不可达。

事件来源：

- 串口输出。
- watchdog。
- reset/reboot/flash 控制动作。
- crash 正则：`Kernel panic`、`BUG:`、`Oops`、`Call trace`。

通知方式：

- 写 `.dut-serial/target-state`。
- 写 `.dut-serial/statusline-cache`。
- hook 在用户提交 prompt 前读取状态。
- 当状态为 `crashed`、`DUT-off`、`disconnected` 时，主动提示 Agent 下一步处理。

## 8. 服务器 CLI：`dutabo`

### 8.1 设计目标

CLI 运行于服务器项目根路径，读取 `.target.toml`，像 Bootlin Lava/Labgrid 一样操作 DUT：

```bash
dutabo list
dutabo state [--dut rk3576-a]
dutabo serial [--dut rk3576-a]
dutabo reset [--dut rk3576-a]
dutabo uboot [--dut rk3576-a]
dutabo maskrom [--dut rk3576-a]
dutabo uf path/update.img [--dut rk3576-a]
dutabo flash-kernel path/boot.img [--dut rk3576-a]
```

### 8.2 不挤掉 Agent 连接

实现方式：

- 优先连接本项目 MCP HTTP 端口。
- 如果 MCP 未运行，CLI 可启动 HTTP MCP。
- `serial` 实时日志通过 `serial_poll_logs` 或读取 `current.serial.log` 实现。
- 不直接打开 UART TCP 端口，避免抢占 Agent 的串口连接。

### 8.3 多 DUT 选择

- 单 DUT 自动选择。
- 多 DUT 未传 `--dut` 时列出 alias 并让用户选择。
- 非交互场景未传 `--dut` 时报错。

## 9. 烧录抽象

### 9.1 目标

用户在服务器执行：

```bash
dutabo uf path/update.img
```

系统自动完成：

1. 解析软链接，使用真实文件。
2. 上传镜像到 dev host `/tmp` 或配置目录。
3. 校验 sha256，确认无损坏。
4. 检测 loader/MASKROM 状态。
5. 如果处于 MASKROM，先烧录 loader bin。
6. 如果 dev host 有多台 loader 设备，要求人工选择。
7. 按 `.target.toml` 的命令模板烧录整包或内核。

### 9.2 配置

```toml
[flash]
tool = "upgrade_tool"
upload_dir = "/tmp"
full_image_cmd = "uf {image}"
kernel_image_cmd = "di -k {image}"
loader_bin = "/opt/rockchip/loader.bin"
loader_cmd = "db {loader}"
list_devices_cmd = "ld"
```

模板变量：

- `{image}`：dev host 上的镜像路径。
- `{loader}`：dev host 上的 loader bin 路径。
- `{tool}`：烧录工具名。

### 9.3 执行边界

MCP/CLI 不内置 SoC 细节。具体烧录命令由 `.target.toml` 定义，从而支持不同 SoC 和工具。

## 10. MCP 工具扩展

新增或强化工具：

| 工具 | 作用 |
| --- | --- |
| `serial_learn_connection` | 执行三次学习，生成参考日志，建立连接判定 |
| `serial_verify_relay` | CH340 控制读回自检 |
| `serial_button` | 按下/松开 reset/recovery/maskrom |
| `serial_get_state` | 返回 active/booting/uboot/crashed/DUT-off/disconnected |
| `serial_poll_logs` | 实时增量日志，不抢串口 |
| `serial_flash_plan` | 根据配置生成烧录计划 |
| `serial_flash` | 执行上传、校验、烧录 |

现有工具保留：

- `serial_send_command`
- `serial_reset`
- `serial_enter_uboot`
- `serial_enter_maskrom`
- `serial_get_logs`
- `serial_list_logs`
- `serial_load_reference`
- `serial_append_reference`

## 11. inotify 事件驱动

避免轮询：

- 监听 `.target.toml`：配置变化后提示重启 MCP 或热加载安全字段。
- 监听 `current.serial.log`：CLI `serial` 实时显示增量。
- 监听 `target-state`：hook 和 CLI 即时感知状态变化。

限制：

- `.target.toml` 不由 Agent 修改。
- 会改变连接拓扑的字段不热更新，要求重启 MCP。

## 12. 验收清单

### 12.1 学习过程

- [ ] reset 按下后创建带时间戳学习日志。
- [ ] reset 松开后日志保存到学习文件。
- [ ] 连续三次学习日志前 50 行相似度大于 93% 时生成参考日志。
- [ ] CH340 控制读回与发送值一致时判定可控。
- [ ] reset 学习相似度低于 10% 时判定继电器不可用并降级软件 reboot。
- [ ] 无 reset 继电器但有参考日志时，三次 reboot 相似度大于 93% 判定连接建立。

### 12.2 目标板控制

- [ ] reset/recovery/maskrom 都通过 press/release 布尔抽象。
- [ ] `.target.toml` 未定义通道时对应能力不存在。
- [ ] 外部 `dev_ctl` 可执行程序可被配置并调用。
- [ ] 控制后端与 CH340 解耦，预留 SNMP v3 PDU。

### 12.3 多 DUT

- [ ] `.target.toml` 可定义多个 DUT alias。
- [ ] CLI 多 DUT 未指定时要求选择。
- [ ] 每台 DUT 有独立日志目录、状态文件、锁。

### 12.4 服务器命令

- [ ] `dutabo serial` 查看实时日志且不挤掉 Agent。
- [ ] `dutabo reset` 控制 relay reset。
- [ ] `dutabo uf path/update.img` 自动解析软链接、上传、校验并烧录。
- [ ] 多 loader 设备时人工选择。
- [ ] MASKROM 模式下先烧录配置的 loader bin。

### 12.5 Agent

- [ ] Agent 不生成、不修改 `.target.toml`。
- [ ] `.target.toml` 存在但 `.mcp.json` 不存在时自动生成 MCP 配置。
- [ ] 任意 Agent 命令通过 MCP 串口工具执行。
- [ ] Kernel panic/BUG/Oops 进入 `crashed` 状态。
- [ ] `crashed`、`DUT-off`、`disconnected` 主动通知 Agent。

## 13. 分阶段实现

### P0：连接学习与配置对齐

- 补齐 `.target.toml` schema 和兼容字段。
- 实现三次学习和相似度判断。
- 实现软件 reboot fallback。
- 生成并加载 `reference_log`。

### P1：控制抽象

- 将 CH340 reset/maskrom/recovery 接入 `PowerControl`。
- 实现 relay verify。
- 实现 `serial_button`。
- 接入 `dev_ctl` 外部控制程序。

### P2：CLI 与多 DUT

- 实现 `dutabo list/state/serial/reset`。
- 引入 DUT registry 和 alias 选择。
- 每 DUT 独立日志、状态、锁。

### P3：烧录

- 实现 upload + sha256 校验。
- 实现 `uf` 和 kernel flash。
- 实现 MASKROM loader 前置烧录。
- 处理多 loader 设备选择。

### P4：事件驱动与通知完善

- inotify 监听日志、状态、配置。
- hook 主动提示 Agent 状态变化。
- 完善 HTTP MCP 与 CLI 共用连接。
