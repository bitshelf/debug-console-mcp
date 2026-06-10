# Embedded Debug MCP — 实现与维护指南

> 配合 `embedded-debug-mcp-v4.md` 设计文档使用。本文档面向实现者和维护者。

## 文件结构

```
docs/design/embedded-debug-mcp/
├── README.md                        # 用户指南 (快速开始、Tool 参考、故障排除)
├── INTERNALS.md                     # 本文档 (实现与维护)
└── (同目录下的设计文档)
    ├── embedded-debug-mcp-v4.md      # 架构设计
    └── embedded-debug-mcp-v4-review.md # Review 结果
```

运行时文件:

```
{project_root}/
├── .target.conf                     # 用户编写
├── .mcp.json                        # SessionStart hook 自动生成
└── .dut-serial/
    ├── .boot_count
    ├── target-state
    └── logs/
        ├── boot-NNN_YYYYMMDD_HHMMSS.log
        └── serial.current.log → boot-NNN_*.log

~/.claude/
├── skills/embedded-debug/mcp/*.py   # MCP Server 源码
└── hooks/embedded-debug/*.py        # Hook 脚本

/tmp/embedded-debug/locks/
└── {md5_host_port}.lock             # 全局互斥锁
```

## 关键模块说明

### `mcp/server.py` — 入口

- FastMCP 实例化 + Lifespan 定义
- 注册所有 9 个 MCP Tools
- `Lifespan.startup()`: lock check → SerialEngine 初始化 → serial.open()
- `Lifespan.shutdown()`: 清理 → serial.close() → release lock

### `mcp/serial_engine.py` — 核心引擎

协调所有子系统:
- 后台 asyncio 读循环: `loop.run_in_executor(None, serial.read)` → 数据管道
- 数据流: serial.read() → LogManager.write() → BootStageDetector.feed() → CommandQueue.feed_response()
- 看门狗: 指数退避重连 (1→2→4→...→30s) + 挂死检测

### `mcp/console.py` — 串口 I/O

三层结构:
1. `ConsoleProtocol` (ABC) — `_read()`, `_write()`
2. `ConsoleExpectMixin` (Mixin) — `expect()`, `sendline()`, `sendcontrol()`, `settle()`
3. `SerialConsoleDriver` (Driver) — pyserial `_read()`/`_write()` 实现

### `mcp/ptx_expect.py` — pexpect 适配

`PtxExpect(pexpect.spawn)`:
- `send()` → `driver.write()`
- `sendcontrol()` → `string.ascii_lowercase.index(char) + 1`
- `read_nonblocking()` → `driver.read(size, timeout)`
- `expect()` → 直接继承 pexpect 正则引擎

### `mcp/boot_detector.py` — 启动阶段检测

逐行扫描串口输出，匹配预定义阶段 pattern:
- SPL/DDR → `rotate_log`
- autoboot → `send_ctrl_c`
- login: → `send_login`
- Password: → `send_password`
- shell prompt → 设 `booted`
- panic/BUG/Oops → 设 `crashed`

### `mcp/state_manager.py` — 状态管理

状态语义 (statusline 显示):
- `active`:       启动完成，shell 就绪，可执行命令
- `booting`:      启动中 (SPL 检测到，正在启动)
- `uboot`:        U-Boot 交互模式
- `crashed`:      内核崩溃 (panic/BUG/Oops)
- `disconnected`: 连不上 dev host (ser2net 不可达)
- `DUT-off`:      目标卡死，启动中超时

内部状态 (不直接显示):
- `stopped`:      MCP Server 未运行
- `connecting`:   正在建立串口连接 (短暂过渡)

状态转换流程:
1. MCP 启动 → `stopped` (或串口打开失败 → `disconnected`)
2. SPL 检测 → `booting`
3. Shell 检测 → `active` (启动完成)
4. U-Boot 检测 → `uboot`
5. panic 检测 → `crashed`
6. 串口断开 → `disconnected`
7. `booting` 超时 → `DUT-off`

滞后挂死检测: 仅从 `booting` 触发，连续 N 次无输出才切换为 `DUT-off`。
`disconnected` 时不检测 — 可能是网络问题，不是目标板卡死。

### `mcp/log_manager.py` — 日志管理

- 存储: `{project}/.dut-serial/logs/boot-NNN_YYYYMMDD_HHMMSS.log`
- 切割触发: SPL 检测 (自动) / reset (手动) / new_log (手动) / 文件 > 100MB (自动)
- 保留策略: 最多 `RK_MAX_ARCHIVED_LOGS` 个
- 符号链接: `serial.current.log` → 当前日志
- `.boot_count`: 存于 `.dut-serial/` 根目录 (与 logs/ 分离)

### `mcp/command_queue.py` — 命令队列

- 串行化: 同一时间只有一个命令在目标上执行
- marker 模式: `echo {marker}; {cmd}; echo $?; echo {marker}`
- 响应路由: 在串口数据流中扫描 begin/end marker → 路由回对应 Agent 的 Future
- 超时: 默认 90s (与 lava BOOTLOADER_DEFAULT_CMD_TIMEOUT 对齐)

### `mcp/relay_manager.py` — 继电器控制

- 协议: 4 字节命令包 `[0xA0, channel, opcode, checksum]` @ 9600 baud
- 长连接: `_ensure_open()` 复用 serial 连接，避免每次开新 TCP
- 复位: RESET 引脚拉低 500ms → 释放
- MASKROM: 完整序列 + try/finally 回滚保证引脚释放

### `mcp/lock_manager.py` — 互斥锁

- 锁文件: `/tmp/embedded-debug/locks/{md5_host_port}.lock`
- 原子创建: O_EXCL 防止竞态
- 僵尸清理: 检查 PID 是否存活
- 内容: `{PID}\n{host:port}\n{ISO_timestamp}`

### `mcp/marker.py` — 标记符生成

- `gen_marker()`: 10 字符随机大写字母
- 排除 R, I, D: 继承自 labgrid，避免误匹配 ERROR/FAIL/INFO/DEBUG

### `mcp/config.py` — 配置解析

- 解析 shell 风格的 `.target.conf` (key=value)
- 递归向上查找配置文件
- 合并默认值

## Hook 脚本

### `session-start.py`

1. 递归向上查找 `.target.conf`
2. 如找到: 确保 `.mcp.json` 存在，如缺失则自动生成
3. Claude Code 自动读取 `.mcp.json` 并启动 stdio MCP Server

### `statusline.py`

1. 递归向上查找 `.dut-serial/target-state`
2. 文件存在 → 读取状态 → 格式化为 `● serial:{state}` (带颜色)
3. 文件不存在 → 不显示 serial 指示器
4. 严格 <1ms，零网络 I/O，零子进程

### `user-prompt-submit.py`

1. 读 `.dut-serial/target-state`
2. 状态 `crashed`/`DUT-off`/`disconnected` → 输出 `{"systemMessage": "告警"}`
3. 其他 → `{"continue": true}`

### `session-stop.py`

No-op。Claude Code 退出时子进程 MCP Server 自动收到 EOF → 正常 shutdown → 释放资源。

## 设计原则

### 为什么不用 socat

pyserial `serial.serial_for_url("socket://host:port")` 直连 ser2net，与 labgrid 一致:
- 零中间进程 (无 socat 管理开销)
- 零 PTY 伪终端 (无多余内核抽象)
- 连接状态由 pyserial 直接反映 (SerialException / read 返回空)
- labgrid 生产环境验证过的方案

### 为什么不用 SSH 控制继电器

pyserial 直连 ser2net (另一个 TCP 端口) + 4 字节协议:
- 零 SSH 依赖，不需要 Dev Host 上的用户/密钥
- 不需要 `serial_relay` 二进制
- 同一个 pyserial 技术栈

### 为什么 stdio transport

- 生命周期绑定 Claude Code: 启动时 spawn, 退出时自动清理
- 同 host:port 互斥由 lock 文件保证，不是一个 daemon 独占
- 多 Claude Code 实例可以各自连接不同的目标板

### 为什么不用 pexpect 直连串口

pexpect 设计为 spawn 子进程，不支持 pyserial 对象。labgrid 的 `PtxExpect` 方案
通过继承并重写 `send()`/`read_nonblocking()` 获得全部 expect 匹配能力，是已验证的成熟模式。

## 测试要点

### 单元测试

- `StateManager`: 滞后防抖逻辑、原子写、状态过滤
- `LogManager`: 切割逻辑、旧文件清理、符号链接
- `CommandQueue`: 串行化、marker 提取、超时
- `BootStageDetector`: 各阶段 pattern 匹配、回调触发
- `RelayManager`: 命令包构造、checksum 计算
- `LockManager`: 原子创建、冲突检测、僵尸清理

### 集成测试

- serial.open() → 读循环 → 数据写入日志
- 命令执行: sendline → marker 检测 → 响应提取
- U-Boot 中断: Ctrl-C → prompt 检测
- 自动登录: login: → username → shell prompt
- 崩溃检测: kernel panic → state → crashed

### 端到端测试

- 完整 boot cycle: reset → SPL → U-Boot → kernel → login → booted
- 两个 Claude Code 实例同 host:port 互斥
- 模拟串口断开 → 自动重连
- 模拟目标挂死 → DUT-off 检测

## 关键常量和约定

| 常量 | 值 | 来源 |
|------|-----|------|
| `BOOTLOADER_DEFAULT_CMD_TIMEOUT` | 90s | lava_common/constants.py |
| `LINE_SEPARATOR` | `\n` | lava_common/constants.py |
| `LOGIN_INCORRECT_MSG` | "Login incorrect" | lava_common/constants.py |
| `LOGIN_TIMED_OUT_MSG` | "Login timed out" | lava_common/constants.py |
| `DISTINCTIVE_PROMPT_CHARACTERS` | `\:` | lava_common/constants.py |
| Relay HEADER | `0xA0` | serial_relay/src/main.rs |
| Relay baudrate | 9600 | serial_relay/Cargo.toml |
| Marker 长度 | 10 | labgrid util/marker.py |
| Marker 字符池 | A-Z 排除 R,I,D | labgrid util/marker.py |
| 默认波特率 | 1500000 | Rockchip U-Boot 默认 |
| Watchdog 退避 | 1→2→4→...→max 30s | 自定 |
| 挂死超时 | 60s | 自定 (可配置) |
| 挂死滞后 | 3 次 | 自定 (可配置) |
| 日志保留 | 10 个 | 自定 (可配置) |
| 日志文件上限 | 100MB | 自定 (可配置) |
