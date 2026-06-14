# Codex Serial Statusline Audit

Date: 2026-06-23

## Objective

Add a serial status to Codex statusline and port Claude Code's `~/.claude/hooks/embedded-debug/` behavior to Codex.

## Current Implementation

- Project hooks are configured in `hooks.json`.
- Codex hook scripts live in `hooks/codex/`.
- `hooks/codex/statusline.py` and `hooks/codex/serial-status.py` read:
  - `.dut-serial/statusline-cache`
  - `.dut-serial/target-state`
- `~/.codex/config.toml` enables:

```toml
[tui]
status_line = ["project-name", "git-branch", "run-state", "task-progress"]
```

## Claude Hook Coverage

| Claude behavior | Codex implementation | Status |
|---|---|---|
| `SessionStart` starts/ensures embedded-debug MCP | `hooks/codex/session-start.py` | Implemented |
| `PreToolUse` blocks raw serial/relay Bash access | `hooks/codex/pre-tool-use.py` | Implemented |
| `UserPromptSubmit` reports crashed/off/disconnected target states | `hooks/codex/user-prompt-submit.py` | Implemented |
| `statusLine.command` reads serial status | `hooks/codex/statusline.py` and `serial-status.py` | Implemented as command entry points |
| `SessionStop` cleanup | Not installed | Claude version only removes a stale git-cache lock; no Codex equivalent needed for serial state |

## Codex Statusline Limitation

Codex CLI 0.141 exposes a fixed statusline item set. Evidence from the local binary lists built-in items such as:

- `project-name`
- `current-dir`
- `run-state`
- `thread-title`
- `git-branch`
- `context-remaining`
- `context-used`
- `five-hour-limit`
- `weekly-limit`
- `codex-version`
- `used-tokens`
- `total-input-tokens`
- `total-output-tokens`
- `thread-id`
- `fast-mode`
- `model-with-reasoning`
- `reasoning`
- `task-progress`

No `serial` renderer or external command statusline renderer is exposed in Codex 0.141. Because of that, a native Codex statusline item named `serial` cannot be completed from project hooks alone.

## Verified Commands

```bash
codex exec --strict-config 'return exactly ok'
python3 -m py_compile hooks/codex/lib.py hooks/codex/session-start.py hooks/codex/pre-tool-use.py hooks/codex/user-prompt-submit.py hooks/codex/serial-status.py hooks/codex/statusline.py
python3 -m json.tool hooks.json
python3 -m json.tool hooks/codex/hooks.json
```

Temporary project checks verified:

- `hooks/codex/statusline.py --empty-ok` prints `serial:active` when `.dut-serial/target-state` contains `active`.
- `hooks/codex/serial-status.py --empty-ok` prints `serial:active`.
- `hooks/codex/pre-tool-use.py` blocks raw `nc ... 2000` serial access.

## Completion Status

The Claude hook behavior has been ported to Codex project hooks. Serial status is available through Codex hook `statusMessage` and command entry points.

The native Codex statusline item itself remains blocked by Codex 0.141's fixed renderer set. Completing a true `serial` statusline item requires upstream Codex support for either:

- a custom statusline command item, or
- a plugin/API surface for registering statusline renderers.
