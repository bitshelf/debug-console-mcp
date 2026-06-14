# StageLearner 自学习日志分割方案 — 2026-06-18

## 核心目标

用 `strsim` 文本相似度算法（StageLearner）作为**主检测路径**，替代 regex 优先的架构。
当 StageLearner 匹配失败时，收集未分类行，让 **Agent（LLM）** 裁剪关键行追加到
`reference-boot.log`，热重载后 StageLearner 越用越准。

## 背景：当前问题

### 问题 1: TOML 格式错误导致 REFERENCE_LOG 从未加载

`.target.toml` 第 30 行：
```toml
REFERENCE_LOG=.dut-serial/reference-boot.log   # ← shell 格式，不是 TOML
```

TOML 解析器报 `Invalid value (at line 30, column 15)`，整个文件解析失败，
`parse_toml_config` 返回空 HashMap，所有配置丢失（包括 `REFERENCE_LOG`）。
StageLearner 从未启用，回退到纯 regex。

**修复**：改为 `reference_log = ".dut-serial/reference-boot.log"`（TOML 顶层键格式）。

### 问题 2: check_stages 优先级反了

当前 `boot_detector.rs::check_stages`（line 542-581）：
```
1. Regex 匹配 BOOT_STAGES（优先）
2. if events.is_empty() → StageLearner 回退
```

regex 的 `DDR_RE = "DDR\s+Version"` 匹配不了 Rockchip 的 `DDR fdeec6f4fc`。
regex 没匹配 → 走 StageLearner → 但 StageLearner 没加载（问题 1）→ DDR 永远检测不到。

**修复**：翻转优先级——StageLearner 优先，regex 回退。

### 问题 3: DDR 不触发 BootStart

当前只有 SPL 和 U-Boot 触发 `BootStart`（日志分割）。DDR 是最早的重启信号
（板子重启后 DDR init 先于 SPL），但 DDR 阶段 `action: None`，不触发分割。

**修复**：DDR 也触发 BootStart。

### 问题 4: detector.feed 收到含噪声的原始数据

`serial_engine.rs::read_loop_iter`（line 332）：
```rust
let events = self.detector.feed(&data);  // 原始 data，含 \x1b[200~ 等
```

`data` 包含 bracketed paste 转义序列（`\x1b[200~`）、ser2net banner 等，
干扰 StageLearner 的文本相似度计算。而 `logs.write(&clean_data)` 用的是清理后的。

**修复**：`detector.feed(&clean_data)`（用清理后的数据）。

### 问题 5: handle_boot_events 的 BootStart 去重

当前（line 418-431）：
```rust
BootEvent::BootStart => {
    let cur = self.state.current();
    match cur {
        TargetState::Booting | TargetState::UBoot => {}  // ← 忽略！
        _ => { self.logs.flush_boot_log(); ... }
    }
}
```

状态为 Booting/UBoot 时 BootStart 被忽略。但板子从 kernel panic 自动重启时，
状态可能是 Active/Booting——DDR 出现表示新 boot cycle，应该分割。

**修复**：DDR 出现时**总是分割**（无论当前状态），用 `boot_detected` 标志去重同 cycle。

### 问题 6: classify_line 只用 3-gram Jaccard

当前 `classify_line`（line 266-309）只用自实现的 3-gram Jaccard 相似度。
对短行（如 "In"）匹配率低；Jaccard 对部分匹配不敏感。

strsim 0.11 提供更强算法：`jaro_winkler`（前缀加权）、`sorensen_dice`（bigram）、
`normalized_levenshtein`（编辑距离）。

**修复**：组合分数 `jaccard * 0.6 + jaro_winkler * 0.4`。

### 问题 7: reference log 含 bracketed paste 噪声

reference-boot.log 第 1 行以 `\x1b[200~` 开头（bracketed paste mode），
会被提取为 DDR 指纹的锚点行，污染相似度计算。

**修复**：`from_reference_text` 加载时 strip ANSI + bracketed paste。

### 问题 8: is_boot_like 阈值太低

`serial_engine.rs::watchdog_once`（line 400）：
```rust
if learner.is_boot_like(&text, 0.10) {  // ← 0.10 太低
```

0.10 几乎任何文本都判为 "boot-like"，导致误判重启。

**修复**：提高到 0.25。

---

## 架构设计

### 检测流程（新）

```
串口数据 → strip_ser2net_banner + strip_ansi → detector.feed(clean_data)
  │
  └─ check_stages(line)
       │
       ├─ 1. 崩溃检测（regex，始终执行）
       │     panic/BUG/Oops/segfault → BootEvent::Crash(type, line)
       │     （通用模式，不依赖 reference log）
       │
       ├─ 2. StageLearner 优先（如果已加载）
       │     classify_line(line) → 组合分数 jaccard*0.6 + jaro_winkler*0.4
       │     ├─ 匹配成功 → BootEvent::Stage(name)
       │     │              ├─ "ddr"/"spl" → 也触发 BootEvent::BootStart
       │     │              └─ return（不跑 regex 阶段，避免重复）
       │     └─ 匹配失败 → 收集到 unclassified_lines
       │
       ├─ 3. login/password（regex，始终执行——需要 action）
       │     login: → BootEvent::LoginPrompt（触发 send_login）
       │     Password: → BootEvent::PasswordPrompt（触发 send_password）
       │
       └─ 4. 其他 regex 阶段（仅当 StageLearner 没匹配时）
             kernel/shell/autoboot/android_* → BootEvent::Stage(name)
             （reference log 可能不含这些阶段，regex 作为补充）
```

### 未分类行收集

```
StageLearner 没匹配 + regex 也没匹配的行
  → BootStageDetector.unclassified_lines.push(line)
  → 每收到 20 行未分类行，或检测到阶段边界（learner/regex 匹配成功）
    → BootEvent::Unclassified(self.unclassified_lines.drain(..).collect())
  → serial_engine 收到后写入 .dut-serial/unclassified.log
```

### Agent 自学习工作流

```
1. Agent 调用 serial_get_unclassified()
   → {"lines": ["DDR fdeec6f4fc typ 23/09/25...", "In", "LP4/4x derate en..."], "count": 15}

2. Agent 分析：这些是 DDR init 阶段的输出

3. Agent 裁剪关键行（锚点行，如 "DDR fdeec6f4fc typ..."）

4. Agent 调用 serial_append_reference(lines="DDR fdeec6f4fc typ 23/09/25...")
   → 打开 reference-boot.log（append 模式）
   → 写入 lines
   → self.detector.load_reference(path)  // 热重载
   → {"success": true, "fingerprints": 42}  // 指纹数增加

5. 下次串口数据进来 → StageLearner 用新指纹匹配 → DDR 被正确检测
```

### BootStart 分割逻辑（新）

```
BootEvent::BootStart => {
    // DDR/SPL 出现 = 板子重启 = 新 boot cycle
    // 无论当前状态如何，都分割日志（用 boot_detected 去重同 cycle）
    if !self.detector.boot_detected {
        self.logs.flush_boot_log();
        self.logs.mark_boot_start();
        tracing::info!("BootStart: new boot cycle (was {:?})", cur);
    }
    self.state.transition(TargetState::Booting);
    self.detector.reset_cycle();
}
```

`boot_detected` 在 `handle_stage`/`check_stages` 里设为 true（DDR/SPL 匹配时），
`reset_cycle` 时重置为 false。这样同 cycle 内多个 DDR/SPL 行只触发一次分割。

---

## 修改清单

### P0 — 必须修复

| # | 文件 | 改动 |
|---|------|------|
| 1 | `/media/loh/powereuler/powereuler-embedded_v4/.target.toml` | `REFERENCE_LOG=...` → `reference_log = ".dut-serial/reference-boot.log"` |
| 2 | `boot_detector.rs` `check_stages` | 翻转：learner 优先 → regex 回退。learner 匹配成功时 `return`（不跑 regex 阶段）。learner 没匹配时继续 regex（kernel/shell/autoboot 等） |
| 3 | `boot_detector.rs` `check_stages` learner 分支 | DDR 也触发 BootStart：`stage_name == "ddr" \|\| stage_name == "spl"` → push BootStart |
| 4 | `boot_detector.rs` `BOOT_STAGES` | DDR 加 `action: Some("rotate_log")`（regex 回退路径也触发 BootStart） |
| 5 | `boot_detector.rs` `DDR_RE` | 扩展 `r"DDR\s+(?:Version\|f[0-9a-f]{8}\|Init)"` |
| 6 | `serial_engine.rs` `handle_boot_events` BootStart | DDR 出现时**总是分割**（无论当前状态），用 `boot_detected` 标志去重同 cycle |
| 7 | `serial_engine.rs` `read_loop_iter` | `detector.feed(&data)` → `detector.feed(&clean_data)`（用清理后的数据） |
| 8 | `boot_detector.rs` `BootEvent` | 新增 `Unclassified(Vec<String>)` 变体 |
| 9 | `boot_detector.rs` `check_stages` | learner 没匹配且 regex 也没匹配的行 → 收集到 `unclassified_lines`，每 20 行或阶段边界时发 `BootEvent::Unclassified` |
| 10 | `serial_engine.rs` `handle_boot_events` | 处理 `BootEvent::Unclassified` → 写入 `.dut-serial/unclassified.log` |
| 11 | `mcp.rs` tool 定义 | 新增 `serial_get_unclassified` + `serial_append_reference` |
| 12 | `serial_engine.rs` | 新增 `get_unclassified() -> Value` + `append_reference(lines: &str) -> Value`（追加 + 热重载） |

### P1 — 高优先级

| # | 文件 | 改动 |
|---|------|------|
| 13 | `boot_detector.rs` `classify_line` | 组合分数：`jaccard * 0.6 + jaro_winkler * 0.4` |
| 14 | `boot_detector.rs` `is_boot_like` 阈值 | 0.10 → 0.25（watchdog 重启检测） |
| 15 | `boot_detector.rs` `from_reference_text` | 加载时 strip bracketed paste（`\x1b[200~`/`\x1b[201~`）和 ANSI 转义 |

### P2 — 文档

| # | 文件 | 改动 |
|---|------|------|
| 16 | `references/.target.toml.example` | 加 `reference_log` + `pass`/`maskrom_channel` 注释语义 |
| 17 | `skill/SKILL.md` | 配置说明 + 自学习工作流说明 |
| 18 | `mcp-rs/README.md` | 同步更新 |

---

## TOML 配置语义规则

| 配置项 | 注释（`#`） | 设置空值（`""`） | 设置非空值 |
|--------|------------|------------------|-----------|
| `pass` | 不设置（键不存在，用默认值） | 密码为空（明确设为空字符串） | 密码为该值 |
| `maskrom_channel` | MASKROM 继电器不控制（`maskrom_ch=0`） | — | 继电器通道号 |
| `reference_log` | 不加载参考日志（StageLearner 不启用） | — | 加载该路径的参考日志 |

### 修正后的 `.target.toml`

```toml
# Embedded Debug Target Configuration (TOML)
# Dev Host 192.168.1.105 bridges ttyACM0 via ser2net TCP 2000

[dev_host]
ip = "192.168.1.105"
user = "linaro"
# pass = ""        # 注释 = 不设置；pass = "" = 密码为空

[serial]
port = 2004

[target]
login_user = "root"
# login_pass = "ubuntu"   # 注释 = 不设置

[uboot]
# interrupt_char: "ctrl_c" for Rockchip, "2" for Allwinner
interrupt_char = "ctrl_c"
interrupt_strategy = "aggressive"

[relay]
port = 2004
reset_channel = 1     # user channel0 → protocol ch1 → SRST
# maskrom_channel = 2   # 注释 = MASKROM 继电器不控制

[monitor]
hang_timeout = 60
max_archived_logs = 10

# StageLearner: 参考启动日志路径（启用跨 SOC 自适应阶段检测 + 日志分割）
reference_log = ".dut-serial/reference-boot.log"
```

---

## 新 MCP Tool 定义

### `serial_get_unclassified`

```json
{
  "name": "serial_get_unclassified",
  "description": "Get serial output lines that StageLearner could not classify into any known boot stage. Use this to identify new boot patterns, then call serial_append_reference to add them to the reference log for future detection.",
  "input_schema": {"type": "object", "properties": {}}
}
```

返回：
```json
{
  "lines": ["DDR fdeec6f4fc typ 23/09/25...", "In", "LP4/4x derate en..."],
  "count": 15,
  "log_path": ".dut-serial/unclassified.log"
}
```

### `serial_append_reference`

```json
{
  "name": "serial_append_reference",
  "description": "Append key anchor lines to the reference boot log and hot-reload StageLearner. Use after analyzing unclassified lines from serial_get_unclassified. The lines become new fingerprints — StageLearner will match them on future boot cycles without restart.",
  "input_schema": {
    "type": "object",
    "properties": {
      "lines": {
        "type": "string",
        "description": "Key anchor lines to append (newline-separated). Pick distinctive lines that mark a boot stage boundary (e.g. 'DDR fdeec6f4fc typ...', 'U-Boot SPL board init', 'Linux version 5.10.0')."
      }
    },
    "required": ["lines"]
  }
}
```

返回：
```json
{
  "success": true,
  "message": "Appended 3 lines, reloaded reference log",
  "fingerprints": 42,
  "reference_log_path": ".dut-serial/reference-boot.log"
}
```

---

## strsim 算法组合

### 当前（只用 3-gram Jaccard）

```rust
let anchor_sim = jaccard_similarity(&line_grams, &fp.anchor_grams);
let score = anchor_sim;
```

### 改进（Jaccard + Jaro-Winkler 组合）

```rust
let jaccard = jaccard_similarity(&line_grams, &fp.anchor_grams);
let jaro = strsim::jaro_winkler(line, &fp.anchor);
let score = jaccard * 0.6 + jaro * 0.4;
```

**为什么这个组合更好：**

| 算法 | 优势 | 劣势 |
|------|------|------|
| 3-gram Jaccard | 长行匹配好（DDR training data） | 短行匹配差（"In" vs "DDR"） |
| Jaro-Winkler | 短行前缀匹配好（"U-Boot SPL" vs "U-Boot SPL board init"） | 长行计算慢 |

组合后：
- 长行（DDR training data）：Jaccard 主导（3-gram 匹配）
- 短行（"U-Boot SPL"）：Jaro-Winkler 主导（前缀匹配）
- 部分匹配行：两者互补

---

## 验证步骤

1. `cargo build && cargo test` — 编译测试通过
2. 修改 `.target.toml` 后重启 MCP
3. 检查 `mcp.log`：`Auto-loaded reference log` + `StageLearner loaded: N fingerprints`
4. 做一次 `serial_reset` → 验证日志正确分割（每个 boot cycle 一个 `boot-NNN.log`）
5. 调用 `serial_get_unclassified()` → 验证未分类行收集
6. 调用 `serial_append_reference(...)` → 验证热重载 + 指纹数增加
7. 再次 `serial_reset` → 验证新指纹被使用（分割更准确）

---

## 文件影响清单

| 文件 | 改动类型 |
|------|---------|
| `mcp-rs/src/boot_detector.rs` | 核心改动：check_stages 翻转 + DDR BootStart + Unclassified 事件 + strsim 组合 + reference 清理 |
| `mcp-rs/src/serial_engine.rs` | feed clean_data + handle_boot_events BootStart 逻辑 + Unclassified 处理 + get_unclassified/append_reference 方法 |
| `mcp-rs/src/mcp.rs` | 新增 2 个 tool 定义 + handler |
| `.target.toml`（项目实际文件） | TOML 格式修复 |
| `references/.target.toml.example` | 模板更新 |
| `skill/SKILL.md` | 文档更新 |
| `mcp-rs/README.md` | 文档更新 |
