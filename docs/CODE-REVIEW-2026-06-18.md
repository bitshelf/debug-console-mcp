# Code & Documentation Review — 2026-06-18

> **Scope**: `mcp-rs/` (current Rust), `mcp-python/` (legacy), `hooks/claude/` (active
> hooks), `scripts/`, `skill/`, `references/`, and all documentation.
> **Mode**: Read-only review (this report was produced before any fixes).
> **Reference baseline**: `AGENTS.md` (comments/docs in English),
> `mcp-rs/Cargo.toml` (`dead_code/unused_* = "deny"`), root `README.md`.

## Executive Summary

| Dimension | Verdict |
|-----------|---------|
| Architecture | Good. Event-driven + marker-echo + StageLearner adaptive detection, superior to reference. |
| Core module test coverage | Decent (`mcp.rs`, `state_manager.rs`, `console.rs`, `marker.rs`). |
| Production-path test fidelity | **Poor** — tests use `open_current`, production uses `ensure_current_file`; paths diverge. |
| Comment/doc language | **Convention violated widely** — most Rust comments are Chinese; `DEPLOY.md`/`TESTING.md` fully Chinese. |
| Doc/code consistency | **Heavy drift** — tool counts, filenames, protocol version, FastMCP claims all inconsistent. |
| Dead code | **Abundant** — `mcp-python/`, `scripts/` are essentially dead; multiple `#[allow(dead_code)]` bypass deny-level lints. |
| Existing internal review docs | `mcp-python/docs/code-review*.md` report several P0/P1 that **are already fixed in code** but the docs were not updated — actively misleading. |

---

## P0 — Must fix (data correctness / functional bugs)

### P0-1 `log_manager.rs` — symlink truncation empties archived boot logs
`mark_boot_start()` (lines 164-167) calls `flush_boot_log()` then `start_new_cycle()`:
- `flush_boot_log` (lines 201-208): writes ring buffer to `boot-NNN.log`, then
  `remove_file(serial.current.log)` + `symlink(boot-NNN.log, serial.current.log)`.
- `start_new_cycle` (lines 170-178): `current_file.take()` then
  `OpenOptions::create(true).write(true).truncate(true).open(serial.current.log)`.

On Linux, `open(O_CREAT|O_WRONLY|O_TRUNC)` **follows the symlink** by default, so it
truncates the just-written `boot-NNN.log` to 0 bytes. Result: every archived
`boot-NNN.log` ends up empty/misaligned; the most recent flush's data is lost.
Tests do not check `boot-NNN.log` *content*, so this passes CI silently.

### P0-2 `bin/statusline-watch.rs` — inotify watch breaks on atomic-write rename
The daemon watches the file `statusline-cache` directly (lines 112-115,
mask=`MODIFY|CLOSE_WRITE|CREATE`). But `state_manager.rs::atomic_write`
(lines 263-272) uses `write(tmp)+rename(tmp→path)`. Linux inotify binds to the
**inode**; after `rename`, the watch stays on the old (now unlinked) inode and
the new inode is **not** watched. The mask also lacks `MOVED_TO`. After the
first atomic write the daemon goes deaf.

Worse: the active `hooks/claude/statusline.py` reads the project-local
`.dut-serial/statusline-cache` **directly**, not the `/dev/shm` mirror; and the
MCP already writes `/dev/shm` itself (`state_manager.rs`). So the daemon's
mirror is **consumed by nobody** — dead infrastructure.

### P0-3 `command_queue.rs` — timeout in `feed_serial_data` does not dequeue next
`feed_serial_data` (lines 107-114) timeout branch does `self.current.take()` +
`resolve()` but **not** `self.dequeue_next()`; `check_timeouts` (line 180+) does.
When data arrives after a timeout, the next pending command is never dispatched
until an external caller invokes `check_timeouts`/`execute` — queue deadlock.

### P0-4 `lock_manager.rs` — cross-user mutex defeat + unbounded recursion
- `process_alive` (lines 82-87): `kill(pid,0)==0` only is treated as alive. `EPERM`
  (process exists but no permission to signal) returns -1 and is misclassified as
  a zombie → lock deleted → mutex defeated. Should be `ret==0 || errno==EPERM`.
- `acquire_lock` race branch (line 53) recurses with no bound → stack overflow;
  and the recursive call uses `to_str().unwrap_or(default_dir)`, so non-UTF8 paths
  silently create locks in the wrong directory.

---

## P1 — High priority

### P1-1 `relay_manager.rs` — responses not drained + `maskrom_ch` unchecked + dead branch
- ON/OFF commands write the packet, `sleep(50ms)`, return `Ok(Vec::new())`
  (lines 94-116) **without reading** the response. CH340 relay replies accumulate
  in the TCP kernel buffer → backpressure / next-write failure.
- `configured()` (lines 38-40) only validates `reset_ch`; `maskrom_ch` is not
  range-checked (1-4). `maskrom_ch=5` sends an invalid channel byte.
- `OP_STATUS` branch (line 107) is logically unreachable; `OP_TOGGLE`
  (lines 24-25) is fully dead, suppressed by `#[allow(dead_code)]`.
- `reset()` has no rollback (MASKROM path does).

### P1-2 `log_manager.rs` — regex recompiled every write + full-file read
- `strip_ansi_and_null` (line 350) calls `Regex::new()` on **every** `write()`.
  High-throughput serial data makes this expensive. Should use `LazyLock`
  (as `command_queue.rs` already does).
- `read_log` (line 271) does `fs::read_to_string` + `lines().collect()` on the
  whole file. A 100MB log OOMs. Should stream from tail.

### P1-3 `mcp_http.rs` — incomplete + zero tests + security
- `tower-http` (cors) is a dependency but is **never used** → no CORS middleware,
  browser clients blocked.
- No `Mcp-Session-Id` session management, no SSE streaming response, no auth,
  default bind `0.0.0.0:3000` (anyone on the network can drive the DUT).
- Background tasks not joined on shutdown; serialization failure returns empty 200.
- Comment claims "event-driven, no polling" but code `sleep(10ms)` — contradictory.
- Zero tests.

### P1-4 `hooks/claude/pre-tool-use.py` — interception is a no-op
- On match it returns `"continue": True` (lines 102-111) — **still allows execution**,
  only attaches `systemMessage`. To block it must be `continue: false`.
- Relay raw-byte regex `\\\\x[0-9a-fA-F]` (lines 55-56) is literal `\\x` in regex,
  **never matches** `\xa0`. Rule is dead.
- Regexes are easily bypassed (`/usr/bin/nc`, variable concatenation, etc.).

### P1-5 `hooks/claude/session-start.py` — unvalidated kill + does not generate .mcp.json
- `_kill_stale_mcp_on_port` (lines 82-95) `os.kill`s whatever PID owns the port
  **without checking comm** — can kill unrelated processes. Compare
  `scripts/start-mcp.sh` (validates) and `user-prompt-submit.py` (validates):
  three inconsistent kill paths.
- README claims "SessionStart hook auto-generates `.mcp.json`" but this hook only
  **reads** an existing `.mcp.json`. In stdio mode it does not start the MCP at
  all (Claude Code spawns it). Doc/code contradiction.
- `find_projects` only checks CWD, does not walk up — inconsistent with
  `lib.find_project_dir`.

### P1-6 Shell injection (copy-pasted in two files)
`hooks/claude/statusline.py:80-87` and `mcp-python/hooks_statusline.py:90-95` both
build `bash -c f'cd "{os.getcwd()}" ... > "{cache}"'`. Paths containing `"`/`$`/
backticks are interpreted by the shell. Should use `subprocess.run([...], cwd=...)`.

### P1-7 `Cargo.lock` gitignored for a binary project
`mcp-rs/.gitignore` ignores `Cargo.lock`. Rust convention: **binary projects must
commit `Cargo.lock`**. Non-reproducible builds; `deploy.sh` also lacks `--locked`.

### P1-8 `command_queue.rs` — exit code extraction is fragile
`output.lines().rev()` scans **all** lines (lines 157-166); any integer-only line
(e.g. `wc -l` output `42`) can be misread as the exit code. `rfind(stripped)`
searches the whole output for the substring (so `"127"` matches `/tmp/127`).
`parse::<i32>()` accepts negatives, but `$?` is 0-255.

---

## P2 — Medium / cleanup

**Rust**
- `state_manager.rs`: `Booted`/`Connecting` states never reached in production
  (`#[allow(dead_code)]` bypasses lint); `on_activity` does not reset
  `last_probe_time` → can premature-DUT-off; `project_hash` duplicated in
  `bin/statusline-watch.rs:19` (MD5) with no shared module; `from_str` is
  test-only but public.
- `console.rs::drain_writes` re-enqueues data on write failure (line 109) → can
  reorder or loop.
- `boot_detector.rs`: `shell` regex `[#\$]\s*$` is too broad (U-Boot, comments,
  `VAR=$` false-positive); `StageFingerprint.prefix/suffix` are
  `#[allow(dead_code)]`.
- `config.rs`: `DEV_HOST_USER/PASS` are parsed but **never used** (SSH removed);
  `references/.target.conf.example` still lists them → misleading;
  `.target.toml.example` misses `hang_hysteresis`; `get_int` is
  `#[allow(dead_code)]`.
- `mcp.rs`: `serial_uboot_command` is fire-and-forget (`serial_engine.rs:485`
  does not truly await the prompt) — semantic gap with "send at U-Boot prompt".
- `lock_manager.rs`: `/proc` is Linux-only and not annotated; `extern "C"`
  should be `extern "C-unwind"` under Rust 2024 edition.
- `relay_manager.rs`: `reset()` has no rollback (MASKROM path does).

**Python / hooks**
- `user-prompt-submit.py`: `_restart_mcp` kills any process whose comm contains
  "python" on port 3000 (misfire); `fuser 3000/tcp` hardcodes the port;
  `_check_ser2net` blocks 3s.
- `statusline.py` docstring lies: "no daemon, no inotify, zero polling" — it is
  polled ~1s by Claude Code and `session-start.py` spawns the
  `statusline-watch` daemon.
- `hooks/claude/lib.py` and Rust `config.rs` maintain **two copies** of
  `PROJECT_MARKERS`/`FORBIDDEN_ROOTS`.

**Dead/damaged code**
- `mcp-python/`: `config.py` uses `RK_*` prefixed keys + hardcoded
  `192.168.1.189`, **incompatible** with the current `.target.conf`/Rust (Rust
  strips `RK_`, Python does not) → legacy MCP connecting to the current config
  hits the wrong IP; `hooks_session_start.py` references a non-existent
  `daemon.py`; `start.sh` references a non-existent `.venv`; `hooks_lib.py`
  duplicates the active `lib.py`.
- `scripts/`: `libmonitor.py` self-marked DEPRECATED; `symlink_scripts` is an
  11-byte **text file** (not a symlink — broken); `start-mcp.sh` has no
  references; `config.py` defaults conflict with `mcp-python/config.py`.
- `mcp-python/README.md`/`INTERNALS.md` mention FastMCP (not used), list
  `fastmcp`/`attrs` deps (not declared and not used), tool count 10 vs actual 9.
- `mcp-python/docs/code-review.md`: P0-1/P0-2/P1-1/P1-5 **are already fixed in
  code** but the doc still reports them as "must fix" → misleading.

---

## Documentation issues

| File | Issue |
|------|-------|
| `mcp-rs/README.md` | Module table lists `relay.rs` (actual: `relay_manager.rs`); dependency table missing `toml/strsim/axum/tower-http/inotify/md-5/once_cell`. |
| `mcp-rs/DEPLOY.md` | Fully Chinese; tool count says 10 in one place and 15 in another; architecture diagram has `relay.rs` (non-existent); no mention of `--http` or `statusline-watch`. |
| `mcp-rs/TESTING.md` | Fully Chinese; troubleshooting says `touch target-state` triggers inotify (daemon watches `statusline-cache` — wrong). |
| `README.md` (root) | "SessionStart hook auto-generates `.mcp.json`" contradicts code; statusline attribution to inotify in `statusline.py` (actually in the daemon). |
| `skill/SKILL.md` | Missing `serial_reboot_uboot`; "stdio spawned by hook" wording is inaccurate. |
| `mcp-python/docs/code-review-rs.md` | Lives under `mcp-python/docs/` (wrong place); P2-1 says `md5_hash` is misleading — it was renamed `fnv1a_hash`; doc stale. |
| All Rust sources | Comments are mostly Chinese, violating the "comments in English" convention (`statusline-watch.rs` is the only fully-English exception). |

---

## Strengths (worth keeping)

- `marker.rs` tests are excellent; `mcp.rs`/`state_manager.rs`/`console.rs`
  protocol/state/IO coverage is good.
- StageLearner cross-SOC adaptive detection is a highlight.
- New TOML config + shell fallback is well designed and tested.
- `serial_reboot_uboot` (soft reboot + Ctrl-C flood) is a clever complement for
  `bootdelay=0`.
- Atomic state-file writes, lock zombie cleanup, hysteresis debounce are solid.
- `code-review-rs.md`'s labgrid/lava comparison is valuable (but needs refresh).

---

## Fix priority

1. **Immediate (P0)**: log_manager symlink truncation, statusline-watch inotify
   (or delete the daemon if confirmed dead), command_queue timeout dequeue,
   lock_manager EPERM/recursion.
2. **Near-term (P1)**: relay response drain + maskrom_ch validation,
   log_manager regex caching + streaming read, mcp_http CORS/session/auth/tests,
   pre-tool-use real interception, session-start kill validation + doc alignment,
   shell injection, commit Cargo.lock.
3. **Cleanup (P2)**: delete `mcp-python/`+`scripts/` dead code, remove redundant
   `#[allow(dead_code)]`, translate comments to English, refresh all drifted
   docs, extract `project_hash` into a shared module.

---

## Decisions applied for the fix pass

- **Dead code**: delete `mcp-python/` and `scripts/` outright.
- **statusline-watch daemon**: delete (its `/dev/shm` mirror is consumed by
  nobody; MCP already writes `/dev/shm`; active statusline reads the project
  file directly).
- **mcp_http.rs**: implement full Streamable HTTP (CORS, `Mcp-Session-Id`,
  SSE streaming, optional token auth, tests).
- **Comments**: translate **all** Rust comments to English per `AGENTS.md`.
