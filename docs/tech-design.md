# Serial Debug MCP 技术设计

> 日期：2026-06-22
> 状态：实施设计稿
> 范围：连接学习、日志分割、目标板控制抽象、多 DUT、服务器 CLI、烧录抽象、Agent 集成约束。

## 1. 目标

本设计将 `sermcp` 扩展为一套可被 Agent 和人工 CLI 同时使用的 DUT 控制与调试系统：

- 自动学习启动日志分割参考，判断串口连接是否真实建立。
- 将 reset、recovery、maskrom 抽象为“按下/松开”的按钮控制，不绑定 CH340。
- 支持 CH340 继电器自检，不可用时自动退化到软件 `reboot`。
- 支持多台 DUT 同时在线，通过别名选择。
- 提供服务器侧 `dutabo` 风格命令：串口、reset、状态、烧录。
- 烧录动作通过 `.target.jsonc` 中的命令模板抽象，支持不同 SoC。
- Agent 只能读取 `.target.jsonc`，不得生成或修改该文件（创建/更新用 `dutabo init`）。

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

- 一个项目目录只能运行一个 MCP 进程；多个 Code Agent 共享该进程时，首个会话
  可操作，后续会话由 MCP 服务端强制为只读。
- 串口连接由 MCP engine 长期持有；CLI 查看实时日志时通过 MCP/HTTP 或日志文件订阅，不挤掉 Agent。
- 硬件控制能力是可选能力；缺失时明确降级，而不是假定存在。
- `.target.jsonc`（JSONC，支持 `//` 注释）是唯一人工配置入口；Agent 只读。
- 日志分割基于学习得到的参考文本，正则仅用于崩溃、登录提示、U-Boot 提示符等强语义事件。

## 3. 配置设计

### 3.1 配置发现

1. `TARGET_CONF` 环境变量显式指定（设置但文件缺失 → 报错）。
2. 从当前目录向上查找 `.target.jsonc`；未找到 → 纯"缺少配置"错误，退出失败。
3. 严格 JSONC 解析（禁止单引号/十六进制数/未知键，拼写错误立即报错）；
   解析失败时报错退出，绝不静默回退。

`.mcp.json` 缺失、或存在但 `mcpServers` 下没有 `sermcp` 条目时，
`dutabo init` 自动创建/补齐该条目（其余 key 原样保留；已有条目不动）；
SessionStart hook 也会在生成 `.mcp.json` 后启动 MCP。

### 3.2 只读约束

Agent 禁止生成、修改 `.target.jsonc`。允许操作：

- 读取配置。
- 生成 `.mcp.json`。
- 生成 `.dut-serial/*` 下的运行时文件。
- 生成学习日志和 `reference_log` 指向的参考日志文件。

### 3.3 单 DUT 示例

```jsonc
// Embedded Debug Target Configuration
// Agent must not edit this file. Create/update via `dutabo init`.

{
  "dev_hosts": [
    {
      "ip": "192.168.1.105",
      "user": "linaro",
      "pass": "",
      "duts": [
        {
          "dut_name": "rk3576-a",
          "serial": {
            "ip": "",         // empty means parent dev host ip
            "port": 2000
          },
          "relay": {
            "ip": "",         // empty means parent dev host ip
            "port": 2001,
            "reset_ch": 1
            // "maskrom_ch": 2,
            // "recovery_ch": 3
          },
          "target": {
            "login_user": "root",
            "login_pass": ""
          },
          "monitor": {
            "hang_timeout": 60,
            "max_archived_logs": 10,
            "reference_log": ".dut-serial/reference-boot.log"
          },
          "flash": {
            "tool": "upgrade_tool",
            "upload_dir": "/tmp",
            "full_image_cmd": "uf {image}",
            "kernel_image_cmd": "di -k {image}",
            "loader_bin": "/opt/rockchip/rk3576_loader.bin",
            "loader_cmd": "db {loader}"
          }
        }
      ]
    }
  ]
}
```

兼容字段（serde alias）：

- `reset_ch` 与 `reset_channel` 等价。
- `maskrom_ch` 与 `maskrom_channel` 等价。
- `recovery_ch` 与 `recovery_channel` 等价。
- `monitor.reference_log` 与顶层/单 DUT `reference_log` 等价。
- 顶层/单 DUT `dut_dir` 为后向兼容键；DUT 缺省 `.dut-serial/<dut_name>`。
- 未知键解析报错（`deny_unknown_fields`）；`port`/`baudrate` 接受数字或字符串。

### 3.4 多 DUT 示例

```jsonc
{
  "dev_hosts": [
    {
      "ip": "192.168.1.105",
      "user": "linaro",
      "pass": "",
      "duts": [
        {
          "dut_name": "rk3576-a",
          "serial": { "port": 2000 },
          "relay": { "port": 2001, "reset_ch": 1, "maskrom_ch": 2 },
          "monitor": { "reference_log": ".dut-serial/rk3576-a/reference-boot.log" }
        },
        {
          "dut_name": "rk3588-b",
          "serial": { "port": 2010 },
          "relay": { "port": 2011, "reset_ch": 1 },
          "monitor": { "reference_log": ".dut-serial/rk3588-b/reference-boot.log" }
        }
      ]
    }
  ]
}
```

选择规则：

- 只有一台 DUT：默认选中。
- 多台 DUT：CLI 需要 `--dut <dut_name>`，未指定时交互选择。
- MCP 实例默认绑定当前项目的默认 DUT；后续可扩展工具参数 `dut_name`。

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

学习日志属于临时诊断数据：每个文件最多保留 2 MiB，每个 DUT 最多保留
20 个文件且目录总量不超过 16 MiB；每次写入前后按文件名时间顺序清理最旧文件。
`reference_log` 独立保存，不受该清理策略影响。

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

### 6.2 后端（plugin 注册表）

电源控制后端是可插拔 **plugin**：实现 `PowerControl` trait 后调用
`register_backend("名称", factory)` 注册，引擎/MCP 层零修改。当前注册的后端：

| 后端 | 当前状态 | 用途 |
| --- | --- | --- |
| CH340 relay (`ch340-relay`) | 内置 plugin | reset/maskrom/recovery 按键模拟 |
| software reboot | 引擎能力 | 无 relay 时软件重启 |
| SNMP v3 PDU | 预留 plugin | 未来电源控制 |
| GPIO/button simulator | 预留 plugin | 本机 GPIO 或外部控制程序 |

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
- 写 `.dut-serial/inventory.json`（原子 tmp+rename；Python hooks 的唯一数据源，
  含 per-DUT 预格式化 `state_text`/`state_plain`、`critical` 标记和端口信息）。
- hook 在用户提交 prompt 前读取状态。
- 当状态为 `crashed`、`DUT-off`、`disconnected` 时，主动提示 Agent 下一步处理。

## 8. 服务器 CLI：`dutabo`

### 8.1 设计目标

CLI 运行于服务器项目根路径，读取 `.target.jsonc`，像 Bootlin Lava/Labgrid 一样操作 DUT：

```bash
dutabo list [--json]
dutabo init [--non-interactive] [--quick] [--advanced] [--force]
dutabo status [--dut <board_name>] [--watch]
dutabo serial [--dut <dut_name>]
dutabo reboot [--dut <dut_name>]
dutabo uboot [--dut <dut_name>]
dutabo button <name> <press|release|pulse> [delay_ms]
#   SoC-agnostic: <name> 可以是任意已配置按钮（reset/maskrom/recovery/...）
dutabo uf path/update.img [--dut <dut_name>]
dutabo flash-kernel path/boot.img [--dut <dut_name>]
```

### 8.2 不挤掉 Agent 连接

实现方式：

- 优先连接本项目 MCP HTTP 端口。
- 如果 MCP 未运行，CLI 可启动 HTTP MCP。
- `serial` 实时日志通过 `serial_poll_logs` 或读取 `current.serial.log` 实现。
- 不直接打开 UART TCP 端口，避免抢占 Agent 的串口连接。

### 8.3 多 DUT 选择

- 单 DUT 自动选择。
- 多 DUT 未传 `--dut` 时列出 dut_name 并让用户选择。
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
7. 按 `.target.jsonc` 的命令模板烧录整包或内核。

### 9.2 配置

```jsonc
"flash": {
  "tool": "upgrade_tool",
  "upload_dir": "/tmp",
  "full_image_cmd": "uf {image}",
  "kernel_image_cmd": "di -k {image}",
  "loader_bin": "/opt/rockchip/loader.bin",
  "loader_cmd": "db {loader}",
  "list_devices_cmd": "ld"
}
```

模板变量：

- `{image}`：dev host 上的镜像路径。
- `{loader}`：dev host 上的 loader bin 路径。
- `{tool}`：烧录工具名。

`list_devices_cmd` 完全由配置传入，不内置任何工具名：Rockchip 用
`upgrade_tool ld`，Allwinner 用 `sunxi-fel list`（无 FEL 设备时退出码非零、
stdout 为空，探测按"空设备表"处理），fastboot/rkdeveloptool 等同样适用。
TEST 按钮的探测只对前后两次输出的行做数字归一化 XOR：设备计数等头部行
（`connected(1)`→`connected(2)`）自动抵消，只有真正新出现的设备行会被判为
"本 DUT 进入烧录模式"；其他板子已在烧录模式时其行在两次捕获中相同、同样抵消。

### 9.3 执行边界

MCP/CLI 不内置 SoC 细节。具体烧录命令由 `.target.jsonc` 定义，从而支持不同 SoC 和工具。

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

现有工具保留：

- `serial_send_command`
- `serial_reset`
- `serial_enter_uboot`
- `serial_get_logs`
- `serial_list_logs`
- `serial_load_reference`
- `serial_append_reference`

## 11. notify 事件驱动

仅监听 `target-state`，让 hook 和 CLI 即时感知 DUT 状态变化，避免轮询。
实现使用跨平台 `notify` crate；Linux 上由其 inotify 后端提供事件。

`.target.jsonc` 对 Agent 只读，缺失时 MCP 启动失败；watcher 不监听配置变化。
串口日志由现有流式通道处理，也不纳入文件 watcher。

限制：

- `.target.jsonc` 不由 Agent 修改，由用户通过 `dutabo init` 修改并确认。
- 会改变连接拓扑的字段不热更新，要求重启 MCP。

## 12. MCP Tasks（SEP-2663）

`serial_reset(wait_boot=true)` 是当前的长任务入口。声明
`io.modelcontextprotocol/tasks` capability 的客户端会立即收到
`resultType: "task"` 句柄，随后用 `tasks/get` 轮询；未声明 Tasks 的客户端
走同步兼容路径。`wait_boot=false` 属于短操作，仍直接返回普通工具结果。

任务状态机直接复用 rmcp：`tasks/update` 忽略未知或已经消费的 input key；
`tasks/cancel` 只确认取消意图，worker 真正观察到取消后才进入 `cancelled`。
工具运行失败以 `completed + result.isError=true` 返回，只有 JSON-RPC 执行错误
使用 `failed`。每个 MCP 进程只允许一个 DUT 长任务并发运行。

## 13. 验收清单

### 13.1 学习过程

- [ ] reset 按下后创建带时间戳学习日志。
- [ ] reset 松开后日志保存到学习文件。
- [ ] 连续三次学习日志前 50 行相似度大于 93% 时生成参考日志。
- [ ] CH340 控制读回与发送值一致时判定可控。
- [ ] reset 学习相似度低于 10% 时判定继电器不可用并降级软件 reboot。
- [ ] 无 reset 继电器但有参考日志时，三次 reboot 相似度大于 93% 判定连接建立。

### 13.2 目标板控制

- [ ] reset/recovery/maskrom 都通过 press/release 布尔抽象。
- [ ] `.target.jsonc` 未定义通道时对应能力不存在。
- [ ] 控制后端通过 plugin 注册表接入（`register_backend`），预留 SNMP v3 PDU。

### 13.3 多 DUT

- [ ] `.target.jsonc` 可定义多个 DUT dut_name（dut_name 跨 host 全局唯一）。
- [ ] CLI 多 DUT 未指定时要求选择。
- [ ] 每台 DUT 有独立日志目录、状态文件、锁。

### 13.4 服务器命令

- [ ] `dutabo serial` 查看实时日志且不挤掉 Agent。
- [ ] `dutabo reset` 控制 relay reset。
- [ ] `dutabo uf path/update.img` 自动解析软链接、上传、校验并烧录。
- [ ] 多 loader 设备时人工选择。
- [ ] MASKROM 模式下先烧录配置的 loader bin。

### 13.5 Agent

- [ ] Agent 不生成、不修改 `.target.jsonc`。
- [ ] `.target.jsonc` 存在但 `.mcp.json` 不存在时自动生成 MCP 配置。
- [ ] 任意 Agent 命令通过 MCP 串口工具执行。
- [ ] Kernel panic/BUG/Oops 进入 `crashed` 状态。
- [ ] `crashed`、`DUT-off`、`disconnected` 主动通知 Agent。

## 14. 分阶段实现

### P0：连接学习与配置对齐

- 补齐 `.target.jsonc` schema 和兼容字段。
- 实现三次学习和相似度判断。
- 实现软件 reboot fallback。
- 生成并加载 `reference_log`。

### P1：控制抽象

- 将 CH340 reset/maskrom/recovery 接入 `PowerControl`。
- 实现 relay verify。
- 实现 `serial_button`。
- 控制后端 plugin 注册表（新增后端无需修改引擎代码）。

### P2：CLI 与多 DUT

- 实现 `dutabo list/state/serial/reset`。
- 引入 DUT registry 和 dut_name 选择。
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
