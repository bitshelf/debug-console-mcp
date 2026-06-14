# MCP Server Code Review — v0.1 vs labgrid + lava_dispatcher

> **日期**: 2026-06-10  
> **范围**: `mcp/*.py` (excl. tests, ~1440 lines)  
> **参考**: labgrid `v1.14` (462 lines) + lava_dispatcher `master` (2098 lines)

---

## 1. 🔴 P0 — 必须修复

### P0-1: `command_queue.py` marker 与 `begin_marker` / `end_marker` 相同

**文件**: `command_queue.py:35-40`  
**参考**: labgrid `ubootdriver.py:78`

```python
# 当前代码:
@property
def begin_marker(self) -> bytes:
    return self.marker.encode()

@property
def end_marker(self) -> bytes:
    return self.marker.encode()  # !! 和 begin_marker 完全相同 !!
```

**问题**: `begin_marker` 和 `end_marker` 返回的是完全相同的 10 字节 marker 字符串。`feed_serial_data()` 中先查找 `begin_marker`，然后立即在 `pc.buffer` 中查找 `end_marker`。由于两者相同，**第一次找到 begin_marker 时 data.find(end_marker) 立即匹配自身**，导致 `buffer = data[idx_begin + len(pc.begin_marker):]` 中的剩余内容被解释为 end_marker 之前的输出，输出始终为空字符串。

labgrid 的方式是**同一个 marker 出现两次**— 第一次作为 begin，第二次作为 end，两者完全一致。关键在于 labgrid 的处理方式不同：

```python
# labgrid: 获取 begin→end 之间的全部输出
data = data[data.index(marker) + 1:]
data = data[:data.index(marker)]
```

labgrid 先找到第一个 marker，skipping past it，**然后在剩余数据中找第二个 marker**，所以两个 marker 相同是 OK 的。

我们的代码问题出在 `feed_serial_data` 的流程：
1. `idx_begin = data.find(pc.begin_marker)` → 找到 marker 位置
2. `pc.buffer = data[idx_begin + len(pc.begin_marker):]` → buffer = marker 之后的内容
3. `idx_end = pc.buffer.find(pc.end_marker)` → 在 buffer 中找第二个 marker
4. 但如果 marker 第一次出现的位置就在 `data` 的开头附近，而上一个 chunk 已经处理过它了，`begin_sent=True` 时第一个 `data.find(pc.begin_marker)` 可能匹配到**还未到达的真正的 end_marker**（因为 buffer 拼接逻辑混乱）

**严重性**: 极高 — **发送命令永远不会返回有效输出**。

---

### P0-2: `_read()` 对 `max_size=None` 不处理

**文件**: `console.py:136-148`  
**参考**: labgrid `serialdriver.py:71-88`

```python
# 当前代码:
def _read(self, size: int = 1, timeout: float = 0.0,
          max_size: int | None = None) -> bytes:
    reading = max(size, self._serial.in_waiting)
    if max_size:  # ← None 时跳过, size 作为唯一限制
        reading = min(reading, max_size)
```

```python
# labgrid:
def _read(self, size: int = 1, timeout: float = 0.0, max_size: int = None):
    reading = max(size, self.serial.in_waiting)
    if max_size:                                     # same guard
        reading = min(reading, max_size)
```

labgrid 所有 `_read()` 调用都明确传入 `max_size` — 要么是默认 `None`（无限制），要么是具体值。我们 `_read_loop` 中调用 `_read(size=1, timeout=0.5, max_size=4096)` 明确传了 `max_size`，所以此处不触发。但当 `_read()` 被其他地方调用（如 PtxExpect）时，`max_size=None` → `if max_size` 为假 → 无上限。这在理论上不是 bug，但与 labgrid 文档说明不符："values 'None' or '0' do not restrict the read length"。

**修改**: 明确检查 `max_size is not None`。

---

## 2. 🟡 P1 — 应该修复

### P1-1: `_on_login_prompt()` 中 `time.sleep(0.3)` 阻塞事件循环

**文件**: `serial_engine.py:234-237`

```python
def _on_login_prompt(self):
    if self._login_user:
        time.sleep(0.3)  # ← 阻塞事件循环 300ms
        self.console.sendline(self._login_user)
```

`on_boot_start()` 中也存在 `time.sleep(0.3)`。

**参考**: lava `LoginAction.run()` 没有 sleep，直接用 `connection.sendline()`。lava 依赖 pexpect 的 `connection.wait()` 机制自然等待。

**修改**::
- 将 `time.sleep(0.3)` 替换为 `await asyncio.sleep(0.3)`（回调需要改为 async 或 spawn task）
- 或者移除 sleep — labgrid 和 lava 都不需要这个 delay

**影响**: 低概率但高影响 — 阻塞事件循环 300ms，在极端场景下可能影响其他工具请求的处理。

---

### P1-2: PtxExpect 的 `expect()` 未被使用

**文件**: `serial_engine.py:250-267 (wait_pattern)` vs `boot_detector.py` 整体

**参考**: labgrid `consoleexpectmixin.py:58-62` + `ubootdriver.py:80`

```python
# labgrid 使用 PtxExpect.expect() 进行模式匹配:
def expect(self, pattern, timeout=-1):
    index = self._expect.expect(pattern, timeout=timeout)
    return index, self._expect.before, self._expect.match, self._expect.after
```

**问题**: 我们构建了 `PtxExpect`（继承 `pexpect.spawn`），但只在 `sendline()` / `sendcontrol()` 使用了它，**从未利用过 `expect()` 的核心能力** — 即 pexpect 的正则匹配引擎和内部缓冲管理。

我们的 `BootStageDetector` 完全独立于 pexpect，用自己的逐行扫描 + callback 机制：
- 优点：可以在数据流中同时检测多个模式（scan-loop 模式）
- 缺点：重复实现了 pexpect 的缓冲管理（`_line_buf`, `_split_line`, 65536 limit 等）
- labgrid 的 `UBootDriver._await_prompt()` 使用 `console.expect([prompt, autoboot, password_prompt, TIMEOUT], timeout=2)` 同时监听多个模式 — 这正是 pexpect 的强项

**评估**: boot_detector 的设计选择是合理的（我们不需要 pexpect 的 buffer 机制），但 `wait_pattern()` 本来可以用 `PtxExpect.expect()` 实现而不是注册临时 watcher。watcher 机制增加了 boot_detector 的复杂度。

---

### P1-3: `command_queue.py` marker 在 shell 命令中的安全性

**文件**: `command_queue.py:128-134`  
**参考**: labgrid `ubootdriver.py:78`

```python
# 当前代码 (已修复 marker 引号):
f"echo '{m[:4]}''{m[4:]}'; {pc.command}; echo \"$?\"; echo '{m[:4]}''{m[4:]}'\n"

# labgrid:
f"""echo '{marker[:4]}''{marker[4:]}'; {cmd}; echo "$?"; echo '{marker[:4]}''{marker[4:]}';"""
```

**潜在问题**: 如果用户命令中包含 `'`（单引号），shell 命令将中断。例如 `serial_send_command("echo 'hello'")` 会生成：

```bash
echo 'QWXYZ'; echo 'hello'; echo "$?"; echo 'QWXYZ'
```

`echo 'hello'` 中的 `'hello'` 会在 shell 展开时出问题（实际上它作为子命令是 OK 的 — 是 `echo 'hello'` 而不是 `'echo 'hello''`）。

**更严重的情况**: `serial_send_command("cat file | grep 'pattern'")` → 命令中的单引号与 marker 外层的单引号冲突。

**修改**: 对命令中的 shell 特殊字符进行转义。参考 labgrid 的处理：labgrid 的 `UBootDriver._run()` 中 `cmd` 参数已经是单条命令，不会包含 shell 元字符。

**评估**: 实际中用户不会通过 `serial_send_command` 发送带复杂 shell 引号的命令（一般只发 `uname -a` 这种简单命令），但理论上存在注入风险。

---

### P1-4: `enter_uboot()` 中 Ctrl-C 发送顺序

**文件**: `serial_engine.py:285-301`

```python
async def enter_uboot(self) -> dict:
    ...
    self.relay.reset()      # 1. 先复位
    self.logs.rotate()
    self.detector.reset_cycle()
    for _ in range(150):    # 2. 然后发 Ctrl-C 150 次
        self.console.sendcontrol("c")
        await asyncio.sleep(0.1)
    result = await self.wait_pattern(r"=>|U-Boot[>]", timeout=30)
```

**参考**: lava `UBootAction` 的 Pipeline 顺序 — `ResetDevice` → `BootloaderInterruptAction`:

```python
# lava: Pipeline
self.pipeline.add_action(ResetDevice(self.job))   # 先复位
self.pipeline.add_action(BootloaderInterruptAction(self.job))  # 等 autoboot 提示
```

**差异**: lava 的 `BootloaderInterruptAction` 会**等待 interrupt_prompt 出现**，然后才发送 Ctrl-C。我们的 aggressive 模式从复位就立即 flood，不等待 autoboot 提示。

**问题**: 从复位到 U-Boot 输出 autoboot 提示之间有 ~3-5 秒（SPL → DDR init → 跳转到 U-Boot）。**这期间的 Ctrl-C 全部浪费了**，因为 UART 还没开始接收。实际上，前 3-5 秒的 Ctrl-C 被丢失，剩余的 10 秒足够覆盖 autoboot 窗口。

**优化**: 增加 `await self.wait_pattern("autoboot|U-Boot.*20\d{2}", timeout=8)` 等待到 U-Boot 启动完成再发送 Ctrl-C。如果预判到 `bootdelay=0`（不打印 autoboot），保留 aggressive 策略。

---

### P1-5: 断开检测中写探测 `_write(b"")` 可能无效

**文件**: `serial_engine.py:191-197`

```python
if _timeout_streak >= _max_timeout_streak:
    _timeout_streak = 0
    try:
        loop.run_in_executor(None, lambda: self.console._write(b""))
    except (pyserial.SerialException, OSError):
        self.state.transition("disconnected")
```

**问题**: 向 TCP socket 发送 0 字节数据（`b""`）不一定会触发错误。TCP 栈将空写入视为 no-op，不会触发 RST 或错误。因此无法通过空写探测连接状态。

**参考**: pyserial TCP socket 断开检测需要：
1. 实际发送有效数据并收到 RST
2. 或通过 `serial.in_waiting` 捕获异常（某些 OS）
3. 或使用 `select`/`poll` 检测 fd 状态

**修改**: 改为发送 1 字节控制字符（如 `\x00`）或直接检查 `_serial.in_waiting`（抛异常表示断开）。

---

## 3. 🟢 P2 — 架构差异分析

### P2-1: Pipeline vs Scan-loop 架构差异

**参考结构**: lava `Pipeline` + labgrid `ContextManager/Strategy`

```python
# lava: UBootAction.populate()
pipeline = Pipeline(...)
pipeline.add_action(UBootSecondaryMedia(...))   # 配置
pipeline.add_action(BootloaderCommandOverlay(...))  # 配置
pipeline.add_action(ConnectDevice(...))          # 连接 (SSH/telnet)
pipeline.add_action(ResetDevice(...))            # 复位
pipeline.add_action(BootloaderInterruptAction(...))  # 中断
pipeline.add_action(BootloaderCommandsAction(...))   # 命令
pipeline.add_action(AutoLoginAction(...))        # 登录
pipeline.add_action(ExpectShellSession(...))     # Shell
pipeline.add_action(ExportDeviceEnvironment(...))   # 测试
```

```python
# labgrid: UBootDriver.on_activate()
def on_activate(self):
    if self._status == 0:
        self._await_prompt()                     # 等待并进入 U-Boot
```

**我们的方法**: `BootStageDetector` 逐行扫描 + callback 触发。

**架构含义**:

| 维度 | lava/labgrid | 我们的方法 |
|------|-------------|-----------|
| 执行模型 | 顺序 Pipeline, 每个 Action 完成才到下一个 | 事件驱动, 任意模式匹配即可触发 |
| 状态管理 | Action 持有 `connection.prompt_str` | StateManager 全局状态 |
| 错误处理 | 每个 Action 有自己的 `timeout_exception` | 全局 `wait_pattern` 超时 |
| 重试 | `RetryAction` 包装 Pipeline | `_watchdog_loop` 重连 |
| 故障隔离 | 每个 Action 可以独立失败和重试 | 故障检测全在 boot_detector 中 |

**意义**: 我们的架构更适合**持续监控**场景（长时间保持连接，随时检测状态变化），lava 更适合**一次性任务流**（部署→启动→测试→收集）。这是正确的架构选择。

---

### P2-2: labgrid `Driver` lifecycle 的实现取舍

**参考**: labgrid `Driver` 提供完整生命周期:

```
create → bind (n times, to resources/drivers) → activate → [usage] → deactivate
```

每个阶段都有内置检查 (`check_bound`, `check_active`)。我们只实现了 `activate/deactivate`。

**差异**: `SerialConsoleDriver` 没有继承 labgrid 的 `BindingMixin`（不需要 resource 绑定），实现简化是正确的。

---

### P2-3: lava `LoginAction` 的 kernel message 检查

**参考**: lava `LoginAction.check_kernel_messages()` — 在等待登录提示时同时监听 kernel 错误模式，如果检测到 panic/BUG 则记录测试结果。

```python
connection.prompt_str = LinuxKernelMessages.get_init_prompts()
connection.prompt_str.extend(prompts)
```

我们的 `BootStageDetector._check_crash()` 在**所有行**上运行（不只是在登录阶段），所以不需要 lava 的阶段性检查。实际上我们做得更全面。

---

### P2-4: `timeout` 设置顺序

**文件**: `console.py:125`
**参考**: labgrid `serialdriver.py:84`

```python
# 当前代码:
def _read(self, ...):
    ...
    self._serial.timeout = timeout  # 在 read() 之前设置
    res = self._serial.read(reading)

# labgrid:
def _read(self, ...):
    ...
    self.serial.timeout = timeout
    res = self.serial.read(reading)
```

**一致**: 我们的实现与 labgrid 完全一致。不需要修改。

---

## 4. 测试覆盖分析

| 模块 | 测试文件 | 覆盖率 | 评价 |
|------|---------|--------|------|
| `marker.py` | `test_marker.py` | ✅ 4 tests | 完整 |
| `state_manager.py` | `test_state_manager.py` + smoke | ✅ 8 tests | 完整 (lifecycle, hang, filter, persistence, stale detection) |
| `lock_manager.py` | `test_lock_manager.py` + smoke | ✅ 4 tests | 完整 (acquire, conflict, release, different ports) |
| `relay_manager.py` | `test_relay_manager.py` + smoke | ✅ 7 tests | 完整 (packet, not_configured) |
| `boot_detector.py` | `test_boot_detector.py` + smoke | ✅ 14 tests | 完整 (all stages, crash, watchers, reset) |
| `command_queue.py` | smoke | ✅ 4 tests | 缺少 marker 提取单元测试 |
| `console.py` | smoke | ✅ 3 tests | 缺少 `_read()` / `_write()` 集成测试 |
| `serial_engine.py` | smoke | ✅ 2 tests | 缺少完整 lifecycle 测试 |
| `server.py` | — | ❌ 0 | MCP JSON-RPC 协议层未测试 |
| `config.py` | smoke | ✅ 3 tests | 完整 |

**漏洞**: 缺少对 serial 断开→重连→继续工作的端到端测试。

---

## 5. Review 总结

| 编号 | 文件 | 等级 | 描述 |
|------|------|------|------|
| P0-1 | command_queue.py | 🔴 P0 | `begin_marker` == `end_marker` — marker 提取逻辑有问题 |
| P0-2 | console.py | 🔴 P0 | `max_size` guard 应使用 `is not None` |
| P1-1 | serial_engine.py | 🟡 P1 | `time.sleep(0.3)` 阻塞事件循环 |
| P1-2 | serial_engine.py | 🟡 P1 | PtxExpect.expect() 未被利用 |
| P1-3 | command_queue.py | 🟡 P1 | 用户命令中的单引号可能破坏 shell |
| P1-4 | serial_engine.py | 🟡 P1 | enter_uboot Ctrl-C 空窗期浪费 |
| P1-5 | serial_engine.py | 🟡 P1 | 空写探测 TCP 断开不可靠 |
| P2-1 | 架构 | 🟢 P2 | Pipeline vs Scan-loop — 设计中已确认的正确选择 |
| P2-2 | console.py | 🟢 P2 | Driver 生命周期简化 — 设计合理 |
| P2-3 | boot_detector.py | 🟢 P2 | 崩溃检测优于 lava — 全时段覆盖 |
| P2-4 | console.py | ✅ | timeout 设置顺序与 labgrid 一致 |

**关键发现**: **P0-1 是功能性 bug** — `begin_marker` 和 `end_marker` 返回相同值导致命令响应提取逻辑错误。其他问题为稳健性和架构差异。
