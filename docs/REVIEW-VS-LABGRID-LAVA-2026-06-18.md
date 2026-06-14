# Architecture Review vs labgrid & lava_dispatcher — 2026-06-18

> **References** (cloned at commit from 2026-06-18, then removed):
> - `labgrid-project/labgrid` master — `labgrid/driver/{ubootdriver,shelldriver,serialdriver,consoleexpectmixin,commandmixin,common,manualswitchdriver}.py`, `labgrid/util/{marker,expect,timeout}.py`, `labgrid/protocol/{resetprotocol,consoleprotocol,commandprotocol}.py`, `labgrid/strategy/ubootstrategy.py`, `labgrid/step.py`
> - `Linaro/lava` master — `lava_dispatcher/{action,logical,shell,connection}.py`, `lava_dispatcher/actions/boot/{__init__,bootloader,u_boot}.py`, `lava_dispatcher/connections/serial.py`
>
> **Scope**: For each major design dimension, compare the current `mcp-rs`
> implementation against labgrid/lava, identify gaps, and decide whether to
> adopt the upstream pattern. Verdicts are concrete and actionable.

## Executive verdict

The current Rust implementation already **matches or exceeds** labgrid/lava on
most *operational* dimensions (per-power-cycle log rotation, StageLearner
adaptive detection, Android boot signals, multi-session lock + PID liveness,
atomic state file). Where it can clearly "do better" by borrowing from
upstream is **architecture**, not features:

| Dimension | labgrid | lava | mcp-rs (current) | Adopt upstream? |
|-----------|---------|------|------------------|-----------------|
| Layered protocol ABCs (Console/Command/Reset/FileTransfer) | ✅ strong | ✅ (Action pipeline) | ❌ flat `SerialEngine` | **Yes (selective)** |
| Strategy / state-transition object | ✅ `UBootStrategy.transition(Status)` | ✅ Pipeline + RetryAction | ⚠️ implicit in `handle_boot_events` | **Yes** — expose explicit transitions |
| `expect()` multi-pattern wait | ✅ `expect([p1,p2,TIMEOUT])` | ✅ `prompt_str` list | ⚠️ single-pattern watcher | **Yes** — multi-pattern watcher |
| `check_active` guard on every driver method | ✅ decorator | ✅ via pipeline | ❌ none | **No** — Rust borrow checker + single engine |
| `step`/action logging | ✅ `@step` decorator | ✅ Action.name/results | ⚠️ ad-hoc tracing | **Partial** — structured step logging |
| Retry as first-class (`RetryAction`/`poll_until_success`) | ✅ | ✅ | ❌ one-shot | **Yes** — add retry to `serial_wait_pattern` |
| `force_prompt_wait` (newline-provoke on timeout) | ✅ | ✅ | ⚠️ heartbeat only | **Yes** — adopt in `wait_pattern` |
| `settle(quiet_time)` (silence detection) | ✅ | ✅ (post_login_settle) | ❌ | **Yes** — useful before login |
| `_inject_run()` shell function (marker hiding) | ✅ ShellDriver | ❌ | ❌ — marker in `echo` | **Optional** — reduces marker spoofing |
| Character delay / TX chunking | ❌ (txdelay only) | ✅ `character_delay` | ❌ | **Yes** — high-baud boards need it |
| Power/reset as separate protocol | ✅ `PowerProtocol`/`ResetProtocol` | ✅ `ResetDevice`/`ResetConnection` | ⚠️ relay folded into engine | **No** — single relay, no abstraction needed |
| Bootloader interrupt char sequence | ✅ single `interrupt` | ✅ `interrupt_ctrl_list` (multi) | ⚠️ single char | **Yes** — adopt `interrupt_ctrl_list` |
| Kernel-message parsing during boot | ❌ | ✅ `LinuxKernelMessages.parse_failures` | ⚠️ crash regex only | **Yes** — adopt warning parsing |
| DUT-off vs disconnected distinction | ❌ (just TIMEOUT) | ✅ (InfrastructureError) | ✅ already distinct | No (already better) |
| Per-action timeout deadline object | ✅ `Timeout` | ✅ `max_end_time` | ⚠️ per-call `Duration` | **Partial** — deadline helper |
| XMODEM file transfer | ✅ ShellDriver | ❌ | ❌ | **Later** — out of scope for v0.2 |

The rest of this report details the **high-value adoptions** (marked **Yes**
above) with concrete code references, then lists the things to *deliberately
not* copy.

---

## 1. Layered protocol ABCs — **adopt selectively**

### Upstream
labgrid splits the world into protocols (`ConsoleProtocol`, `CommandProtocol`,
`ResetProtocol`, `FileTransferProtocol`, `LinuxBootProtocol`) and drivers that
implement them. `UBootDriver` and `ShellDriver` both bind onto a
`ConsoleProtocol` supplier, so the same serial console can serve a U-Boot
command path and a Linux shell path without either knowing how bytes get to
the wire.

### Current
`SerialEngine` is one god-object holding `console`, `detector`, `state`,
`logs`, `commands`, `relay`. `serial_send_command` and `serial_uboot_command`
both reach into `engine.console` directly.

### Verdict
Don't introduce a full binding/protocol framework — it's heavy for a
single-DUT tool. **But** extract two thin traits so the U-Boot and Linux
command paths stop duplicating marker-echo logic:

```rust
// new: src/command_runner.rs
pub trait CommandRunner {
    async fn run(&self, cmd: &str, timeout: f64) -> CommandResult;
    async fn await_prompt(&self, timeout: f64) -> Result<(), Error>;
}
// UBootRunner and LinuxShellRunner implement it; both use CommandQueue.
```

This mirrors labgrid's `CommandMixin` (used by both `UBootDriver` and
`ShellDriver`) without the binding machinery. Benefit: `serial_uboot_command`
currently does **fire-and-forget** (P1 finding) — making it go through
`CommandRunner::run` would force it to await a real prompt and reuse the
marker-echo path that already works for `serial_send_command`.

---

## 2. Strategy / explicit state-transition object — **adopt**

### Upstream
labgrid `UBootStrategy.transition(Status)` makes the legal state graph
explicit: `unknown → off → uboot → shell`, each transition activating only the
drivers it needs. lava uses `RetryAction` wrapping a `Pipeline` of actions.

### Current
State transitions are scattered: `handle_boot_events` (serial_engine.rs:415)
imperatively calls `state.transition(...)` from inside the read loop;
`serial_enter_uboot`/`serial_reboot_uboot` do their own ad-hoc transitions
inside `mcp.rs`. The graph is implicit and there's no single place that says
"to get to uboot from active, do X".

### Verdict
Introduce a `BootStrategy` enum + transition function that owns the legal
graph. This is **the single highest-value refactor** — it would make the
existing `serial_enter_uboot` / `serial_reboot_uboot` / `serial_reset` tools
share one validated path instead of three hand-rolled ones:

```rust
pub enum Goal { Active, UBoot, Maskrom, Off }
pub fn plan(current: TargetState, goal: Goal) -> Vec<Action> { ... }
```

Then `serial_enter_uboot` becomes "plan(current, UBoot) then execute the
plan", and the three current ad-hoc implementations collapse into one.
lava's `RetryAction` model also implies: **wrap each plan step in a retry
with `failure_retry`**, which the current code doesn't do at all (every tool
is one-shot).

---

## 3. Multi-pattern `expect` watcher — **adopt**

### Upstream
labgrid: `self.console.expect([self.prompt, self.autoboot, self.password_prompt, TIMEOUT], timeout=2)` — one call waits for *any* of several patterns and returns the index. This is what makes `_await_prompt` (ubootdriver.py:130) so compact: a single loop handles prompt / autoboot / password / idle-timeout.

lava: `connection.prompt_str` is a **list**; `raw_connection.expect(self.prompt_str, timeout=...)` does the same.

### Current
`BootStageDetector::add_watcher(pattern, sender)` (boot_detector.rs:378)
takes a single pattern per watcher. `serial_wait_pattern` registers one
pattern and awaits one channel. To wait for "login: OR Password: OR panic"
the caller must register three watchers and `select!` on three receivers.

### Verdict
Add `add_watcher_multi(patterns: &[&str], sender)` returning the **index** of
the matched pattern (labgrid semantics). This directly simplifies
`serial_enter_uboot` (which currently watches `r"=>|U-Boot[>#]"` as one regex
— fine, but can't report *which* prompt) and enables a future
`serial_wait_login` tool that matches login/password/incorrect in one call.
Model the API on labgrid's `expect` return: `(index, before, match, after)`.

---

## 4. `force_prompt_wait` (newline-provoke) — **adopt**

### Upstream
lava `shell.py:332` `force_prompt_wait`: when waiting for a prompt and
timeout approaches, send a newline to provoke a fresh prompt, then retry up
to 6 times at `timeout/10` each. This handles the embedded-real-world case
where kernel logs overlap the login prompt.

labgrid `ShellDriver._await_login` (shelldriver.py:147) does the same: on
TIMEOUT with `before == last_before` (idle), send `""` to probe state.

### Current
`watchdog_once` (serial_engine.rs:364) has a heartbeat probe that sends `""`
on `Active` after `hang_timeout`, but `serial_wait_pattern` does **not**
provoke — it just waits up to `timeout` and gives up. A flaky prompt that
would appear with one newline is reported as "timed out".

### Verdict
Add a `probe_on_timeout: bool` option to `serial_wait_pattern` (default
`true` for prompt-like patterns). On timeout, send `"\n"` and retry with
`timeout/10` up to 6 times — exactly lava's cadence. This will materially
improve `serial_enter_uboot` and `serial_reset(wait_boot=true)` success rates
on noisy Rockchip consoles.

---

## 5. `settle(quiet_time)` — **adopt**

### Upstream
labgrid `ConsoleExpectMixin.settle(quiet_time, timeout)` (consoleexpectmixin.py:64): wait until the console has been silent for `quiet_time` seconds, up to `timeout`. Used by `ShellDriver` via `post_login_settle_time` to avoid matching a prompt that's interleaved with boot output.

### Current
No equivalent. After login is sent, the code immediately watches for the
next stage — which on Android can be interleaved with `init:` logs and
produce a false "shell" match.

### Verdict
Add `serial_settle(quiet_seconds, timeout)` as a new MCP tool and use it
internally after `LoginPrompt`/`PasswordPrompt` events. Trivial to
implement on top of `console.wait_readable` + an activity timestamp.

---

## 6. Retry as first-class — **adopt**

### Upstream
lava `RetryAction` (logical.py:21): wraps an inner pipeline, retries up to
`failure_retry` times with `failure_retry_interval` sleep, clears child
results between attempts, re-raises on final failure.

labgrid `CommandMixin.poll_until_success` (commandmixin.py:38) and
`wait_for` (line 19): poll a command until exit code matches or timeout.

### Current
Every tool is one-shot. `serial_enter_uboot` fails if the relay glitch
doesn't land; there's no `failure_retry=3`. `serial_reset(wait_boot=true)`
waits for `login:` once.

### Verdict
Add `failure_retry` and `failure_retry_interval` parameters to
`serial_enter_uboot`, `serial_reboot_uboot`, and `serial_reset`. Wrap the
existing logic in a loop that clears detector state between attempts (cf.
lava's `action.results.clear()` at logical.py:104). Default `failure_retry=3`
for the U-Boot tools — this alone will make `serial_enter_uboot` dramatically
more reliable without the current "pray it works" 20s timeout.

---

## 7. `interrupt_ctrl_list` (multi-char interrupt) — **adopt**

### Upstream
lava `BootloaderInterruptAction` (boot/__init__.py:928): `interrupt_ctrl_list`
is a list of control characters sent in sequence before the interrupt char.
Some vendor U-Boots need `\x01` then `\x03` to unlock.

### Current
`config.uboot_interrupt_char()` returns a single `u8`. The flood loops in
`do_relay_reset_and_flood` and `serial_reboot_uboot` send only that one byte.

### Verdict
Extend `.target.toml`:
```toml
[uboot]
interrupt_char = "ctrl_c"
interrupt_ctrl_list = []  # e.g. ["a"] for Allwinner pre-unlock
```
And in the flood loop, send each `interrupt_ctrl_list` char once per cycle
before the interrupt char. Low-risk, high-compat win.

---

## 8. Kernel-message parsing during boot — **adopt**

### Upstream
lava `LinuxKernelMessages.parse_failures` (called from LoginAction.run at
boot/__init__.py:89): while waiting for the login prompt, simultaneously
scan for a curated list of kernel warnings/errors (BUG, oops, panic,
soft lockup, RCU stall, hung_task) and record them as results **without**
aborting the wait.

### Current
`boot_detector.rs` detects panic/BUG/Oops/segfault and transitions to
`Crashed` — but only those 7 patterns, and a crash **aborts** the wait.
Soft lockup, RCU stall, hung_task, workqueue stalls are not detected.

### Verdict
Add a `KernelMessages` table in `boot_detector.rs` with two tiers:
- **fatal** (existing): panic/BUG/Oops → `Crashed` (abort wait)
- **warning** (new): soft lockup, RCU stall, hung_task, warning() →
  recorded in state metadata, wait continues

Expose via `serial_get_state` as `kernel_warnings: [...]`. This matches
lava's "record but don't abort" semantics and gives the agent useful
diagnostic info without false `crashed` transitions.

---

## 9. Character delay / TX chunking — **adopt**

### Upstream
lava `LoginAction.run` (boot/__init__.py:157): `connection.sendline(username, delay=self.character_delay)`. labgrid `SerialDriver` has `txdelay`/`txchunk` (serialdriver.py:12-14) for the same reason.

### Current
`console.sendline` writes the whole line in one `write_all`. At 1.5 Mbaud
Rockchip consoles with a ser2net in the path, long lines can overflow the
relay's UART FIFO and drop characters.

### Verdict
Add `character_delay_ms` and `tx_chunk_size` to `.target.toml` `[serial]`.
In `sendline`, chunk the line and `sleep(character_delay)` between chunks
(mirroring labgrid `ConsoleExpectMixin.write` at consoleexpectmixin.py:34).
Default 0 (off) for backward compat.

---

## 10. Structured step logging — **adopt partially**

### Upstream
labgrid `@step(title=, args=, result=)` decorator (step.py:197): wraps every
driver method, records start/stop/args/result/exception into a `steps`
registry. Enables the labgrid pytest plugin to show a trace of what the
test did on the target.

### Current
`tracing::info!`/`warn!` calls are ad-hoc. There's no structured "what did
we do to the DUT" trace.

### Verdict
Don't adopt the full `@step` registry — it's pytest-specific. **But** add
a lightweight `Step` event to the MCP `serial_get_state` response:
`recent_steps: [{tool, at, duration_ms, ok}]` (last 20). This gives the
agent a self-audit trail without a new dependency. Trivial: a `VecDeque<Step>`
in `SerialEngine` capped at 20.

---

## What NOT to copy from upstream

These labgrid/lava patterns are **not** worth adopting for this project:

| Pattern | Why not |
|---------|---------|
| `@target_factory.reg_driver` + `bindings = {"port": ...}` | Single DUT, single console, single relay — binding framework is pure overhead. |
| `Driver.check_active` decorator | Rust's `&mut self` + single `SharedEngine` already prevents use-after-deactivate. |
| pexpect / `PtxExpect` | The Rust version correctly uses regex line-scan + channel watchers; pexpect's buffer semantics are a liability, not an asset (the legacy Python code already showed this). |
| `XMODEM` file transfer (ShellDriver) | Out of scope for v0.2; can add later via `dd`+base64 if needed. |
| lava `Pipeline`/`Action` class hierarchy | Far too heavy; the `BootStrategy::plan` function captures the useful 10% (explicit transitions + retry). |
| lava `Job`/`Worker`/`protocols` orchestration | This is a CI scheduler concern; the MCP server is interactive. |
| labgrid `Environment`/`Config` YAML | `.target.toml` already covers this. |
| `proxymanager` (labgrid) | ser2net TCP is direct; no proxy indirection needed. |

---

## Concrete adoption plan (ordered by value/effort ratio)

1. **Retry on U-Boot/reset tools** (§6) — highest value, smallest change.
   Wrap `serial_enter_uboot`/`serial_reboot_uboot`/`serial_reset` bodies in
   a retry loop with `failure_retry=3`. ~30 lines each.
2. **`force_prompt_wait` in `serial_wait_pattern`** (§4) — high value on
   noisy consoles. Add `probe_on_timeout` + 6× `timeout/10` retry. ~20 lines.
3. **Multi-pattern watcher** (§3) — `add_watcher_multi` returning index.
   ~40 lines in boot_detector.rs; simplifies mcp.rs callers.
4. **`BootStrategy::plan`** (§2) — collapse the three ad-hoc U-Boot paths.
   ~80 lines new + refactor; this is the architectural payoff.
5. **Kernel warning tier** (§8) — add 5-6 warning regexes, record without
   aborting. ~30 lines.
6. **`interrupt_ctrl_list`** (§7) — config + flood loop. ~15 lines.
7. **Character delay / TX chunking** (§9) — config + `sendline` chunking.
   ~25 lines.
8. **`serial_settle` tool** (§5) — new tool + internal use after login.
   ~30 lines.
9. **Step trace in `serial_get_state`** (§10) — `VecDeque<Step>` cap 20.
   ~40 lines.
10. **`CommandRunner` trait** (§1) — refactor, not new feature. ~60 lines;
    do last once the above stabilize.

Items 1-3 are clearly worth doing in this pass. Items 4-5 are worth doing
if scope allows. Items 6-10 can be follow-up.

---

## Conclusion

The project is **already at or above labgrid/lava quality** on operational
correctness (log rotation, lock liveness, atomic state, Android detection,
adaptive StageLearner). The gap is **architectural clarity** around the
state-transition graph and **robustness patterns** (retry, provoke-prompt,
multi-pattern expect, kernel-warning tier). Adopting items 1-3 from the
plan above would close the most user-visible gaps (U-Boot entry reliability,
noisy-console prompt detection) with roughly 90 lines of code and no new
dependencies — a high return. The `BootStrategy::plan` refactor (item 4) is
the deeper architectural win and should follow once the retry/provoke
patterns are in place.
