---
description: Cold-reset the DUT and stream the boot log until login prompt, using async serial tasks (non-blocking).
argument-hint: [dut-alias] [timeout-seconds]
---

Cold-reset the target DUT and watch a full boot using the async task tools so you stay responsive while waiting:

1. `serial_task_start({"action": "reset_and_wait", "pattern": "login:", "timeout": $2 || 180})` → capture `task_id`.
2. Poll every ~10s with `serial_task_poll({"task_id": ...})`; stop when `status == "completed"` or `"failed"`.
3. On failure, call `serial_task_follow({"task_id": ...})` to fetch the accumulated serial output and diagnose (look for panic / BUG / Oops / Call trace).
4. On success, report `serial_get_state` and any new boot anomalies vs the reference log.

Prefer async over `serial_reset(wait_boot=true)` whenever the boot may exceed ~30s — blocking a tool call wastes context and risks client-side timeouts.