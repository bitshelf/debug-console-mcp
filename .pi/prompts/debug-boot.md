---
description: Monitor embedded Linux DUT serial boot with async task tracking
---
Start an asynchronous boot monitor on the target DUT.

## Steps

1. Verify the sermcp server is available:

   `serial_get_state`

2. Start a monitored boot cycle:

   `serial_task_start(action="reset_and_wait", timeout=180)`

3. Poll boot progress every 10 seconds:

   `serial_task_poll(task_id="<task_id>")`

4. On completion, report:

   - Boot time
   - Stage transitions observed
   - Any warnings or errors in the boot log
   - Final DUT state (active/crashed/DUT-off)

5. If boot fails:
   - Capture crash logs: `serial_get_logs(lines=200, pattern="panic|BUG|Oops")`
   - Report the failure stage and crash details to the user

## Important

- The first `serial_send_command` after MCP start must be a warmup: `serial_send_command("echo warmup", timeout=3)`
- BusyBox targets may need `printf` instead of `echo` in piped commands
- Do NOT use raw `nc`/`tio`/`screen` to access the serial port
