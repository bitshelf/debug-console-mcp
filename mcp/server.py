#!/usr/bin/env python3
"""Embedded Debug MCP Server — zero-dependency JSON-RPC 2.0 over stdio.

Protocol: MCP (Model Context Protocol) 2024-11-05
Transport: stdio (newline-delimited JSON-RPC 2.0)

No third-party MCP framework.  Just asyncio + json + stdlib.

9 Tools: serial_send_command, serial_get_state, serial_get_logs,
         serial_list_logs, serial_reset, serial_enter_uboot,
         serial_wait_pattern, serial_new_log, serial_poll_logs.
"""

import asyncio
import json
import logging
import os
import re
import sys
import time
from dataclasses import dataclass
from typing import Any, Callable

from config import load_config
from serial_engine import SerialEngine

logger = logging.getLogger("embedded-debug")

MCP_PROTOCOL_VERSION = "2024-11-05"


# ── Tool registry ─────────────────────────────────────────────────────────────


@dataclass
class ToolDef:
    """MCP tool definition with JSON Schema input spec."""

    name: str
    description: str
    input_schema: dict
    handler: Callable[..., Any]
    is_async: bool = False


def _tool_defs(engine_fn: Callable[[], "SerialEngine | None"]) -> list[ToolDef]:
    """Build the 9 tool definitions.

    engine_fn is a callable returning the current engine (or None).
    This indirection avoids capturing a stale global.
    """

    def _not_connected() -> dict:
        return {"error": "Serial not connected — check .target.conf and ser2net"}

    # ── async handlers ──

    async def send_command(args: dict) -> dict:
        e = engine_fn()
        if e is None:
            return _not_connected()
        return await e.send_command(args["command"], float(args.get("timeout", 90)))

    async def get_state(args: dict) -> dict:
        e = engine_fn()
        if e is None:
            return _not_connected()
        return e.get_state_dict()

    async def get_logs(args: dict) -> dict:
        e = engine_fn()
        if e is None:
            return _not_connected()
        return e.logs.read_log(
            archive_index=args.get("archive", 0) or 0,
            lines=args.get("lines", 50),
            pattern=args.get("pattern"),
        )

    async def list_logs(args: dict) -> dict:
        e = engine_fn()
        if e is None:
            return _not_connected()
        archives = e.logs.list_archives()
        return {
            "archives": [
                {"index": i, "filename": a["filename"], "size_bytes": a["size_bytes"]}
                for i, a in enumerate(archives)
            ],
            "current": str(e.logs.current_path) if e.logs.current_path else "",
        }

    async def reset(args: dict) -> dict:
        e = engine_fn()
        if e is None:
            return _not_connected()
        return await e.reset_target(args.get("wait_boot", True))

    async def enter_uboot(args: dict) -> dict:
        e = engine_fn()
        if e is None:
            return _not_connected()
        return await e.enter_uboot()

    async def wait_pattern(args: dict) -> dict:
        e = engine_fn()
        if e is None:
            return _not_connected()
        result = await e.wait_pattern(args["pattern"], float(args.get("timeout", 60)))
        if result["matched"] and args.get("action") == "send_ctrl_c":
            e.console.sendcontrol("c")
        result["elapsed_seconds"] = 0
        return result

    async def new_log(args: dict) -> dict:
        e = engine_fn()
        if e is None:
            return _not_connected()
        e.logs.rotate()
        e.detector.reset_cycle()
        return {"success": True, "filename": str(e.logs.current_path)}

    # poll_since state — kept per-server-instance via closure
    _poll_since = [0.0]

    async def poll_logs(args: dict) -> dict:
        e = engine_fn()
        if e is None:
            return _not_connected()

        since = args.get("since")
        if since is None or _poll_since[0] == 0:
            _poll_since[0] = time.monotonic()
            result = e.logs.read_log(archive_index=0, lines=20)
            return {
                "lines": result["content"].splitlines() if result["content"] else [],
                "since": _poll_since[0],
            }

        event = asyncio.Event()
        new_lines: list[str] = []

        def cb(line: bytes):
            new_lines.append(line.decode(errors="replace"))
            event.set()

        e.detector.add_watcher(re.compile(rb"."), cb)
        try:
            await asyncio.wait_for(event.wait(), timeout=float(args.get("timeout", 10)))
        except asyncio.TimeoutError:
            pass
        finally:
            e.detector.remove_watcher(re.compile(rb"."))
        _poll_since[0] = time.monotonic()
        return {"lines": new_lines, "since": _poll_since[0]}

    # ── schema constants ──

    return [
        ToolDef(
            name="serial_send_command",
            description="Send a shell command to the target and return the output.",
            input_schema={
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Shell command to execute"},
                    "timeout": {"type": "integer", "default": 90, "description": "Timeout in seconds"},
                },
                "required": ["command"],
            },
            handler=send_command,
            is_async=True,
        ),
        ToolDef(
            name="serial_get_state",
            description="Get the current target state and metadata.",
            input_schema={"type": "object", "properties": {}},
            handler=get_state,
            is_async=True,
        ),
        ToolDef(
            name="serial_get_logs",
            description="Retrieve serial log content with optional filtering.",
            input_schema={
                "type": "object",
                "properties": {
                    "lines": {"type": "integer", "default": 50, "description": "Number of lines to return"},
                    "pattern": {"type": "string", "description": "Regex filter pattern"},
                    "archive": {"type": "integer", "default": 0, "description": "Archive index (0=current)"},
                },
            },
            handler=get_logs,
            is_async=True,
        ),
        ToolDef(
            name="serial_list_logs",
            description="List all archived boot logs.",
            input_schema={"type": "object", "properties": {}},
            handler=list_logs,
            is_async=True,
        ),
        ToolDef(
            name="serial_reset",
            description="Hardware reset target via relay and rotate log.",
            input_schema={
                "type": "object",
                "properties": {
                    "wait_boot": {"type": "boolean", "default": True, "description": "Wait for boot to complete"},
                },
            },
            handler=reset,
            is_async=True,
        ),
        ToolDef(
            name="serial_enter_uboot",
            description="Force target into U-Boot interactive prompt.",
            input_schema={"type": "object", "properties": {}},
            handler=enter_uboot,
            is_async=True,
        ),
        ToolDef(
            name="serial_wait_pattern",
            description="Wait until a regex pattern appears in serial output.",
            input_schema={
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex pattern to match"},
                    "timeout": {"type": "integer", "default": 60, "description": "Timeout in seconds"},
                    "action": {"type": "string", "description": "Optional action (e.g. send_ctrl_c)"},
                },
                "required": ["pattern"],
            },
            handler=wait_pattern,
            is_async=True,
        ),
        ToolDef(
            name="serial_new_log",
            description="Manually rotate log without hardware reset.",
            input_schema={"type": "object", "properties": {}},
            handler=new_log,
            is_async=True,
        ),
        ToolDef(
            name="serial_poll_logs",
            description="Get new serial output since last poll (long-polling).",
            input_schema={
                "type": "object",
                "properties": {
                    "since": {"type": "number", "description": "Timestamp from previous poll"},
                    "timeout": {"type": "integer", "default": 10, "description": "Long-poll timeout in seconds"},
                },
            },
            handler=poll_logs,
            is_async=True,
        ),
    ]


# ── MCP server ────────────────────────────────────────────────────────────────


class McpServer:
    """Zero-dependency MCP server — JSON-RPC 2.0 over stdio."""

    def __init__(self, name: str = "embedded-debug", version: str = "0.1.0"):
        self.name = name
        self.version = version
        self._tools: dict[str, ToolDef] = {}
        self._initialized = False
        self._engine: SerialEngine | None = None
        self._write_msg: Callable[[dict], None] | None = None

    # ── tool registration ──

    def register_tools(self, tools: list[ToolDef]):
        for t in tools:
            self._tools[t.name] = t

    # ── public API (for testing without stdio) ──

    async def handle_message(self, message: dict) -> dict | None:
        """Process a JSON-RPC 2.0 message.  Returns response dict or None."""
        jsonrpc = message.get("jsonrpc")
        method = message.get("method")
        params = message.get("params") or {}
        req_id = message.get("id")

        if jsonrpc != "2.0":
            if req_id is not None:
                return self._error(req_id, -32600, "Invalid Request: jsonrpc must be '2.0'")
            return None

        if not method or not isinstance(method, str):
            if req_id is not None:
                return self._error(req_id, -32600, "Invalid Request: missing method")
            return None

        # notifications/initialized — no response
        if method == "notifications/initialized":
            self._initialized = True
            return None

        # All other requests require prior initialize
        if not self._initialized and method != "initialize":
            if req_id is not None:
                return self._error(req_id, -32600, "Not initialized: send initialize first")
            return None

        # Route
        if method == "initialize":
            return self._handle_initialize(req_id, params)
        if method == "ping":
            if req_id is not None:
                return {"jsonrpc": "2.0", "id": req_id, "result": {}}
            return None
        if method == "tools/list":
            return self._handle_list_tools(req_id)
        if method == "tools/call":
            return await self._handle_call_tool(req_id, params)

        if req_id is not None:
            return self._error(req_id, -32601, f"Method not found: {method}")
        return None

    # ── protocol handlers ──

    def _handle_initialize(self, req_id, params: dict) -> dict:
        self._initialized = True
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": self.name, "version": self.version},
            },
        }

    def _handle_list_tools(self, req_id) -> dict:
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "tools": [
                    {
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    }
                    for t in self._tools.values()
                ]
            },
        }

    async def _handle_call_tool(self, req_id, params: dict) -> dict:
        if req_id is None:
            return None  # tool calls are requests, must have id

        name = params.get("name", "")
        args = params.get("arguments") or {}

        tool = self._tools.get(name)
        if tool is None:
            return self._error(req_id, -32602, f"Unknown tool: {name}")

        try:
            if tool.is_async:
                result = await tool.handler(args)
            else:
                result = tool.handler(args)
        except Exception as e:
            logger.exception(f"Tool {name} failed")
            return self._error(req_id, -32603, f"Tool error: {e}")

        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "content": [{"type": "text", "text": json.dumps(result, default=str)}]
            },
        }

    # ── helpers ──

    @staticmethod
    def _error(req_id, code: int, message: str) -> dict:
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {"code": code, "message": message},
        }

    def _write_message(self, msg: dict):
        """Send a JSON-RPC message over stdout."""
        if self._write_msg is None:
            return
        self._write_msg(msg)

    # ── stdio transport ──

    async def _run_stdio(self):
        """Wire up stdin/stdout via thread-pool I/O.

        Uses run_in_executor for reads (works with pipes, files, ttys)
        and direct buffer writes for stdout (fast enough for JSON-RPC).
        """
        loop = asyncio.get_running_loop()
        stdin = sys.stdin.buffer
        stdout = sys.stdout.buffer

        # stdout wrapper with explicit flush
        def write_msg(msg: dict):
            data = json.dumps(msg, default=str).encode() + b"\n"
            stdout.write(data)
            stdout.flush()

        self._write_msg = write_msg

        logger.info(f"[{self.name}] stdio transport ready")

        # Read loop
        try:
            while True:
                line = await loop.run_in_executor(None, stdin.readline)
                if not line:
                    break  # EOF
                try:
                    message = json.loads(line)
                except json.JSONDecodeError as e:
                    logger.warning(f"Invalid JSON: {e}")
                    continue
                response = await self.handle_message(message)
                if response is not None:
                    write_msg(response)
        except (BrokenPipeError, ConnectionResetError):
            logger.info("stdio pipe closed")
        except asyncio.CancelledError:
            pass

    # ── lifecycle ──

    async def _start_engine(self):
        """Initialize SerialEngine from .target.conf."""
        config = load_config()
        if not config.get("_CONFIG_PATH"):
            logger.error(
                f"No .target.conf found. cwd={os.getcwd()}, "
                f"TARGET_CONF={os.environ.get('TARGET_CONF', '(unset)')}"
            )
            return

        try:
            self._engine = SerialEngine(config)
            await asyncio.wait_for(self._engine.start(), timeout=15.0)
        except asyncio.TimeoutError:
            logger.error("SerialEngine.start() timed out after 15s — ser2net unreachable?")
            self._engine = None
        except Exception as e:
            logger.error(f"SerialEngine.start() failed: {e}")
            self._engine = None

    async def _stop_engine(self):
        if self._engine:
            await self._engine.stop()

    async def run(self):
        """Main entry point: start engine → run stdio loop → cleanup."""
        await self._start_engine()
        try:
            await self._run_stdio()
        finally:
            await self._stop_engine()


# ── Entry point ───────────────────────────────────────────────────────────────


def main():
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(message)s",
        stream=sys.stderr,  # never stdout — that's for JSON-RPC
    )
    server = McpServer()
    server.register_tools(_tool_defs(lambda: server._engine))
    asyncio.run(server.run())


if __name__ == "__main__":
    main()
