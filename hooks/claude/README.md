# Embedded Debug Hooks for Claude Code

Serial console monitoring hooks — auto-start MCP server + real-time statusline.

## Files

| Hook | Trigger | Function |
|------|---------|----------|
| `session-start.py` | Session start | Detect `.target.toml` → start MCP HTTP |
| `statusline.py` | 1s refresh | Read MCP statusline cache / target-state |
| `pre-tool-use.py` | Before Bash | Intercept raw serial access → prompt MCP |
| `user-prompt-submit.py` | Before prompt | Alert on DUT-off/disconnected/crashed |
| `session-stop.py` | Session stop | No-op (daemon persists) |
| `lib.py` | Shared | find_project_dir, check_mcp_alive, format_serial_state |

## Deploy

```bash
cp hooks/claude/*.py ~/.claude/hooks/embedded-debug/
```
