# Rust 代码审核修复报告

日期：2026-06-23

## 背景

根据“学习过程、连接建立、目标板控制、服务器调试命令、ch340 继电器连接、.target.toml、Agent”需求，对 `mcp-rs` Rust 实现进行了代码审核和修复。重点检查学习阈值、继电器/控制抽象、多 DUT 配置选择、烧录命令抽象和 CLI 行为。

## 已修复问题

1. 学习相似度阈值不符合需求
   - 问题：`LearnConfig::default()` 和 Cargo metadata 默认值为 `0.90`，需求要求三个日志前 50 行相似度大于 93%。
   - 修复：默认阈值改为 `0.93`，同步更新单元测试期望。
   - 涉及文件：`mcp-rs/Cargo.toml`、`mcp-rs/src/connection_learner.rs`

2. 串口 IP 与继电器 IP 没有解耦
   - 问题：`SerialEngine` 使用 dev host 作为串口和 relay 的统一 host，导致 `.target.toml` 中 `serial.ip`、`relay.ip` 无法按职责生效。
   - 修复：串口连接使用 `Config::serial_ip()`，继电器/CH340 backend 使用 `Config::relay_ip()`，并在 `SerialEngine` 中保存独立 `relay_host`。
   - 涉及文件：`mcp-rs/src/serial_engine.rs`

3. 多 DUT 配置选择未真正作用到 MCP
   - 问题：`dutabo --dut <alias>` 只在 CLI 层选择了 DUT，但启动 MCP 时没有传递 DUT alias，MCP 仍可能加载默认 DUT。
   - 修复：
     - `dutabo` 启动 MCP 时设置 `TARGET_DUT_ALIAS` 和 `TARGET_CONF`。
     - `load_config()` 支持读取 `TARGET_DUT_ALIAS`，并把对应 `[[dut]]` 配置合并到运行配置。
     - `DutConfig::to_config_map()` 补齐 dev host、relay IP/type、串口和登录信息映射。
   - 涉及文件：`mcp-rs/src/bin/dutabo.rs`、`mcp-rs/src/config.rs`

4. reset/maskrom/学习流程仍硬绑定 CH340 relay
   - 问题：部分路径直接调用 `RelayManager` 或 CH340 专用构造，未走 `PowerControlBackend`，不利于未来 SNMP PDU 或外部 `dev_ctl`。
   - 修复：
     - `serial_reset` 改为通过 `PowerControlBackend::pulse(Button::Reset, ...)`。
     - `serial_enter_uboot` 的硬件复位阶段改为通过抽象 backend。
     - `serial_enter_maskrom` 改为抽象的 maskrom press/release + reset pulse 序列。
     - 硬件学习和继电器验证改为使用抽象 backend，不再只支持 CH340。
   - 涉及文件：`mcp-rs/src/serial_engine.rs`、`mcp-rs/src/mcp.rs`

5. 烧录命令没有完整使用 `.target.toml` 抽象
   - 问题：
     - `dutabo flash-kernel` 实际执行仍取 `full_image_cmd`。
     - loader 和设备枚举命令硬编码为 `DB`、`LD`。
     - MCP flash plan 没有返回选中的具体烧录命令。
   - 修复：
     - `FlashConfig` 增加 `loader_cmd`、`list_devices_cmd`。
     - MCP `serial_flash_plan` 支持 `image_type`，返回 `selected_flash_cmd`。
     - `dutabo uf` 使用整包命令，`dutabo flash-kernel` 使用内核命令。
     - loader/list 命令改为读取 `.target.toml` 中的 `loader_cmd`、`list_devices_cmd`，默认分别为 `DB {loader}`、`LD`。
   - 涉及文件：`mcp-rs/src/flash.rs`、`mcp-rs/src/mcp.rs`、`mcp-rs/src/bin/dutabo.rs`

6. CLI 参数校验顺序不合理
   - 问题：未知命令、`uf` 缺少镜像参数、`flash-kernel` 缺少镜像参数时，会先要求 `.target.toml`，导致错误信息偏离用户操作。
   - 修复：先校验命令和必要参数，再加载 `.target.toml`。
   - 涉及文件：`mcp-rs/src/bin/dutabo.rs`

7. 集成测试期望与自动启动 MCP 行为不一致
   - 问题：`dutabo` 设计上会自动启动 MCP，部分 “without_server” 测试仍只接受不可达错误。
   - 修复：测试允许 MCP 自动启动后返回有效 stdout，同时保留不可达/MCP 错误输出的兼容判断。
   - 涉及文件：`mcp-rs/tests/dutabo_tests.rs`

## 验证结果

在 `mcp-rs` 目录执行：

```bash
cargo fmt
cargo check
cargo test
```

结果：

- `cargo check`：通过。
- `cargo test`：通过。
- 单元测试：184 passed。
- `dutabo` 集成测试：19 passed。
- doctest：2 ignored，0 failed。

说明：普通沙箱环境下，本地 TCP mock 测试曾因 `PermissionDenied` 失败；在允许本地 TCP mock 的非沙箱环境重跑后全部通过。

## 仍需后续规划的限制

1. MCP `serial_flash` 仍只返回计划和提示，实际烧录仍由 `dutabo uf` / `dutabo flash-kernel` CLI 执行。
   - 原因：MCP 侧当前没有完整实现 dev host SSH 上传、校验、烧录执行链。
   - 当前状态：已修正 flash plan 和 CLI 执行路径，避免错误烧录命令。

2. `serial_learn_connection` 当前仍在 MCP handler 持有 engine lock 期间执行较长学习流程。
   - 风险：学习期间会降低其它 MCP 请求响应能力。
   - 建议：后续把学习流程拆成后台任务或 lock-released 状态机。

3. `RelayManager` 仍作为 CH340 兼容实现存在。
   - 当前关键入口已经改走 `PowerControlBackend`。
   - 建议：后续逐步收敛旧 CH340 专用 API，只保留 backend 实现层。

