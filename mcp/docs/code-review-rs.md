# Rust MCP Server Code Review — v0.2 vs labgrid + lava_dispatcher

> **日期**: 2026-06-11
> **范围**: `mcp-rs/src/*.rs` (14 文件, ~5150 行)
> **参考**: labgrid `v1.14` (Python) + lava_dispatcher `master` (Python)
> **测试**: 140/140 通过 (8.1s)

---

## 1. 架构对比

| 维度 | labgrid | lava_dispatcher | 本实现 (Rust) |
|------|---------|-----------------|---------------|
| 执行模型 | Driver + Strategy + ContextManager | Pipeline + Action 链 | 事件驱动 + Callback |
| 状态管理 | Driver 状态 (binding + active) | Action 实例持有 connection | StateManager 全局状态机 |
| 命令执行 | `UBootDriver._run()` marker-echo | `ShellCommand` + Connection 读取 | CommandQueue + marker-echo ✅ |
| 串口 I/O | pyserial `serial_for_url("socket://")` | pexpect spawn | tokio TcpStream + channel ✅ |
| 多 session | 无内建支持 | 无内建支持 | 文件锁 + PID 存活检测 ✅ |
| 传输层 | N/A (Python 绑定) | SSH + telnet + serial | stdio + Streamable HTTP ✅ |
| 测试覆盖 | 部分 | 部分 | 全面 (140 tests) ✅ |

**架构评价**: 事件驱动模型对小规模监控场景更合适；lava Pipeline 更适合一次性任务。本实现选择正确。

---

## 2. 模块分析

### 2.1 console.rs — 串口驱动

**对比 labgrid SerialDriver**:

| 特性 | labgrid | 本实现 | 评价 |
|------|---------|--------|------|
| 连接方式 | `serial_for_url("socket://")` | `TcpStream::connect` | ✅ 等价 |
| 写入方式 | `serial.write()` 阻塞 | `try_write()` 非阻塞 + channel | ⚠️ 见 P0-1 |
| 读取方式 | `serial.read()` + timeout | `tokio::time::timeout` + read | ✅ 等价 |
| Nagle | N/A | `set_nodelay(true)` | ✅ 合理 |
| 重连 | N/A (pyserial 内建) | connect() 重建 stream | ✅ |
| Control char | `pexpect.sendcontrol()` | 手动编码 `(idx+1)` | ✅ 等价 |

**P0-1: `try_write` 可能丢失数据**

```rust
// drain_writes — 当前实现
match stream.try_write(&data[offset..]) {
    Ok(n) => offset += n,
    Err(ref e) if e.kind() == WouldBlock => {
        std::hint::spin_loop();  // ← 自旋等待，无 yield
    }
    Err(e) => {
        self.connected = false;  // ← 丢弃所有待发数据！
        return;
    }
}
```

labgrid 使用 `serial.write()` 阻塞写入 (`serialposix.py:647`)，绝不丢数据。当前实现遇到非 WouldBlock 错误就设 `connected=false` 并丢弃 channel 中剩余待发数据。

**修复**: 低级别错误只是重试 (回 channel 队列)，只在高水位持续失败时才断连。

### 2.2 command_queue.rs — 命令队列

**对比 labgrid `UBootDriver._run()`**:

| 特性 | labgrid | 本实现 |
|------|---------|--------|
| Marker 格式 | `echo '{m[:4]}''{m[4:]}'` | 完全一致 |
| 退出码提取 | `data.index(marker)` | 最后一行解析数字 |
| ANSI 清理 | `re_vt100.sub()` | `strip_ansi()` 同逻辑 |
| 超时 | 无内建超时 | 90s 默认 ✅ |

**P1-1: begin_marker 跨 chunk 问题**

```rust
// feed_serial_data — 步骤 1
if !pc.found_begin {
    if let Some(idx) = find_subsequence(&data, &marker) {
        pc.found_begin = true;
        pc.buffer = data[idx + marker.len()..].to_vec();
    } else {
        return;  // ← marker 可能被 split 到两个 chunk！
    }
}
```

labgrid 使用 `data.index(marker)` 在整个已缓冲数据上搜索 (`data` 是完整拼接好的缓冲区)。本实现每个 chunk 独立搜索，10 字节 marker 被 chunk 边界切割时会漏掉。

**严重性**: 低概率 — 4096 字节 chunk 中 10 字节 marker 极不可能被切割。但理论上是 bug。

**修复**: 在 `feed_serial_data` 中维护一个环形缓冲区拼接跨 chunk 数据，或保证 marker 在一个 chunk 内。

### 2.3 boot_detector.rs — 启动检测

**对比 lava `BootloaderInterruptAction` + `AutoLoginAction`**:

| 特性 | lava/labgrid | 本实现 |
|------|-------------|--------|
| SPL 检测 | `U-Boot SPL` | ✅ 一致 |
| autoboot 中断 | `Hit any key` | ✅ 一致 |
| Login 发送 | `connection.sendline(user)` | ✅ 一致 |
| Android 检测 | 无 | ✅ 新增 6 个模式 |

**P1-2: Android kernel log 过滤在每个 read 中重新编译 regex**

```rust
fn strip_android_klog(data: &[u8]) -> Vec<u8> {
    let re = regex::bytes::Regex::new(r"(?m)^\[\s*\d+\.\d+\]\[\s*T\d+\]\s*").unwrap();
    re.replace_all(data, b"" as &[u8]).into_owned()
}
```

`Regex::new()` 在每次调用时重新编译正则。read_once 每 100ms 调用一次 → 每秒 10 次编译。应改为 `LazyLock` / `OnceCell` 静态编译。

**修复**: 使用 `std::sync::LazyLock` 或 `once_cell::sync::Lazy` 缓存编译结果。

### 2.4 state_manager.rs — 状态管理

**对比 lava `lava_dispatcher/action.py` Action 状态**:

| 特性 | lava | 本实现 |
|------|------|--------|
| 状态粒度 | Action 级别 (每个 Action 有独立状态) | 全局状态机 |
| 挂死检测 | Action 超时 | 滞后防抖 ✅ |
| 状态持久化 | 无 | 原子写文件 ✅ |
| 多 session 隔离 | N/A | mcp.pid 存活检测 ✅ |

**P1-3: stopped 状态语义不一致**

lava 的 Action 有 `timeout_exception` 机制；labgrid 的 Driver 用 `@check_active` 守卫。本实现在 `stopped` 时删除状态文件，但测试仍检查 `stopped` 写入文件——已在最新版本修复。

### 2.5 relay_manager.rs — 继电器控制

**对比 `serial_relay/src/main.rs` 4 字节协议**:

| 特性 | serial_relay | 本实现 |
|------|-------------|--------|
| Packet 格式 | [0xA0, ch, op, cksum] | ✅ 完全一致 |
| 重连重试 | 无 | ✅ 两次重试 |
| MASKROM 回滚 | 无 | ✅ try/finally |

**评价**: 高质量实现，优于参考实现。

### 2.6 lock_manager.rs — 互斥锁

| 特性 | 评价 |
|------|------|
| O_EXCL 原子创建 | ✅ |
| 僵尸锁清理 (`kill(pid, 0)`) | ✅ |
| FNV-1a 替代 MD5 | ⚠️ 见 P2-1 |

**P2-1: FNV-1a hash 替代 MD5 — 锁 key 碰撞概率**

```rust
fn md5_hash(input: &str) -> u64 {
    // FNV-1a 64-bit
    let mut hash: u64 = 0xcbf29ce484222325;
    ...
}
```

函数名 `md5_hash` 但实际使用 FNV-1a。`host:port` 组合有限（同一网络内最多几百个），64-bit FNV-1a 碰撞概率可忽略。但命名误导。

---

## 3. 🔴 P0 — 必须修复

| 编号 | 文件 | 描述 |
|------|------|------|
| P0-1 | `console.rs:92` | `try_write` 错误时丢弃 channel 中所有待发数据 |
| P0-2 | `boot_detector.rs` (strip_android_klog) | 每次 read 重新编译 regex |

---

## 4. 🟡 P1 — 应该修复

| 编号 | 文件 | 描述 |
|------|------|------|
| P1-1 | `command_queue.rs:121` | begin_marker 跨 chunk 漏检 |
| P1-2 | `serial_engine.rs:289` | Serial read error 触发 Disconnected 而非尝试重连 |
| P1-3 | `main.rs:197` | engine.start() 在 tokio::spawn 中失败时无恢复机制 |

---

## 5. 🟢 P2 — 改进建议

| 编号 | 文件 | 描述 |
|------|------|------|
| P2-1 | `lock_manager.rs:89` | `md5_hash` 实际是 FNV-1a，命名误导 |
| P2-2 | `console.rs:36` | `connected` 和 `stream.is_some()` 冗余 — labgrid 只用 `_status` |
| P2-3 | `serial_engine.rs` | 缺少定期心跳探活 (labgrid 理念：active = 已验证可交互) |
| P2-4 | `log_manager.rs:168` | `read_log` 对大文件读全量 — 考虑 mmap 或 tail |
| P2-5 | `mcp.rs:343` | tools/call 错误被包装为正常 result — 不符合 JSON-RPC error 语义 |
| P2-6 | `relay_manager.rs:73` | `send_command` 的 for 循环重试可以简化 |

---

## 6. 测试覆盖分析

| 模块 | 测试数 | 评价 |
|------|--------|------|
| `mcp.rs` | 18 | ✅ 协议层完整 (init, tools/list, tools/call, errors) |
| `state_manager.rs` | 17 | ✅ 状态切换、挂死检测、文件写入 |
| `serial_engine.rs` | 15 | ✅ probe, logs, relay, commands |
| `console.rs` | 10 | ✅ connect/close, sendline/sendcontrol, read |
| `relay_manager.rs` | 9 | ✅ packet, channel, reset, maskrom |
| `log_manager.rs` | 13 | ✅ rotation, cleanup, read, symlink |
| `lock_manager.rs` | 9 | ✅ acquire/release, zombie, conflict |
| `boot_detector.rs` | 12 | ✅ stages, crash, watchers |
| `marker.rs` | 7 | ✅ gen, pool, uniqueness |
| `command_queue.rs` | 9 | ✅ format, extraction, serialization |
| `config.rs` | 9 | ✅ parse, defaults, quotes, export |

**缺失测试**:
- `strip_android_klog` 未测试
- `strip_ser2net_banner` 未测试
- 多 chunk marker 提取边界情况
- TCP 断连→重连端到端集成测试
- Android shell prompt 混合 kernel log 的真实数据测试

---

## 7. 与 labgrid 关键差异总结

| labgrid 做法 | 本实现做法 | 差异影响 |
|-------------|-----------|---------|
| Connection 层抽象 (SSH/telnet/serial) | 仅 TCP serial | 够用，无需 SSH（ser2net 已封装） |
| `@check_active` 守卫所有 Driver 方法 | 无 Active 守卫 | 低优先级 — Rust 的 borrow checker 已提供安全 |
| BindingMixin (Resource→Driver 绑定) | 无绑定层 | 简化是合理的，config 即 resource |
| pexpect `expect([pattern1, pattern2])` 同时监听 | 逐行 scan + callback | 逐行扫描对持续监控更优 |
| `RetryAction` 包装 Pipeline | 指数退避重连 | 更简单直接 |
| Poller (多连接复用) | 单连接 tokio::select! | 精简，适合单一目标板场景 |

---

## 8. Review 总结

**总体评价**: 高质量的 Rust 移植，核心功能与 labgrid 一致，在 Android 支持、多 session 隔离、测试覆盖方面甚至超出参考实现。

**关键修复优先级**:
1. P0-1: `try_write` 丢数据 (console.rs)
2. P0-2: 正则重复编译 (strip_android_klog)
3. P1-1: marker 跨 chunk 漏检 (command_queue)

**需要集成测试**: TCP 断连→重连完整流程，Android 真实串口数据回归测试。
