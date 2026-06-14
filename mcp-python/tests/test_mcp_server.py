#!/usr/bin/env python3
"""Tests for McpServer — JSON-RPC protocol, tool schemas, tool dispatch.

Uses a MockEngine to avoid real serial hardware.
All async tests use asyncio.run() pattern (no pytest-asyncio dependency).
"""

import asyncio
import json
from unittest.mock import MagicMock

import pytest

from server import McpServer, ToolDef, _tool_defs


# ── Helpers ────────────────────────────────────────────────────────────────────


def _req(method: str, params: dict | None = None, req_id=1) -> dict:
    msg = {"jsonrpc": "2.0", "method": method, "id": req_id}
    if params is not None:
        msg["params"] = params
    return msg


def _notification(method: str, params: dict | None = None) -> dict:
    msg = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        msg["params"] = params
    return msg


def _init_server_sync(server: McpServer):
    """Run initialize handshake synchronously."""
    async def _go():
        resp = await server.handle_message(_req("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"},
        }))
        assert resp is not None
        assert "result" in resp
        resp2 = await server.handle_message(_notification("notifications/initialized"))
        assert resp2 is None
    asyncio.run(_go())


class MockEngine:
    """Fake SerialEngine for tool dispatch tests."""

    def __init__(self):
        self.state = MagicMock()
        self.state.external_state = "active"
        self.logs = MagicMock()
        self.logs.boot_number = 5
        self.logs.current_path = None
        self.logs.read_log.return_value = {
            "content": "line1\nline2\n",
            "filename": "boot-005.log",
            "total_lines": 2,
            "filtered_lines": 2,
        }
        self.logs.list_archives.return_value = [
            {"filename": "boot-005.log", "size_bytes": 1024, "path": "/tmp/boot-005.log"},
        ]
        self.detector = MagicMock()
        self.console = MagicMock()
        self._state_value = "active"

    async def send_command(self, command: str, timeout: float = 90.0) -> dict:
        return {"output": f"mock: {command}", "exit_code": 0, "timed_out": False}

    def get_state_dict(self) -> dict:
        return {
            "state": self._state_value,
            "boot_number": self.logs.boot_number,
            "last_data_seconds": 0,
            "log_path": "",
            "relay_configured": False,
            "login_configured": True,
        }

    async def wait_pattern(self, pattern: str, timeout: float = 60.0) -> dict:
        return {"matched": True, "matched_line": f"mock matched: {pattern}"}

    async def reset_target(self, wait_boot: bool = True) -> dict:
        return {"success": True, "new_boot_number": 6, "log_path": "/tmp/boot-006.log",
                "boot_complete": True}

    async def enter_uboot(self) -> dict:
        return {"success": True, "state_after": "uboot"}


def _make_server_with_mock() -> tuple[McpServer, MockEngine]:
    server = McpServer()
    mock = MockEngine()
    server.register_tools(_tool_defs(lambda: mock))
    server._engine = mock
    return server, mock


def _make_init_server() -> tuple[McpServer, MockEngine]:
    """Create server + mock, run initialize handshake."""
    server, mock = _make_server_with_mock()
    _init_server_sync(server)
    return server, mock


# ── 1. Protocol: initialize handshake ─────────────────────────────────────────


class TestInitialize:
    def test_initialize_returns_protocol_version(self):
        server = McpServer()
        async def _go():
            resp = await server.handle_message(_req("initialize", {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            }))
            assert resp is not None
            assert resp["id"] == 1
            result = resp["result"]
            assert result["protocolVersion"] == "2024-11-05"
            assert "tools" in result["capabilities"]
            assert result["serverInfo"]["name"] == "embedded-debug"
            assert result["serverInfo"]["version"] == "0.1.0"
        asyncio.run(_go())

    def test_initialized_notification_completes_handshake(self):
        server = McpServer()
        async def _go():
            await server.handle_message(_req("initialize", {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            }))
            resp = await server.handle_message(_notification("notifications/initialized"))
            assert resp is None
            assert server._initialized is True
        asyncio.run(_go())

    def test_request_before_initialize_is_rejected(self):
        server = McpServer()
        async def _go():
            resp = await server.handle_message(_req("tools/list"))
            assert resp is not None
            assert "error" in resp
            assert resp["error"]["code"] == -32600
            assert "Not initialized" in resp["error"]["message"]
        asyncio.run(_go())

    def test_notification_before_initialize_is_silently_dropped(self):
        server = McpServer()
        async def _go():
            resp = await server.handle_message(_notification("tools/list"))
            assert resp is None
        asyncio.run(_go())

    def test_ping_after_initialize(self):
        server, _ = _make_init_server()
        async def _go():
            resp = await server.handle_message(_req("ping", req_id=42))
            assert resp == {"jsonrpc": "2.0", "id": 42, "result": {}}
        asyncio.run(_go())


# ── 2. Protocol: error handling ───────────────────────────────────────────────


class TestProtocolErrors:
    def test_invalid_jsonrpc_version(self):
        server = McpServer()
        async def _go():
            resp = await server.handle_message({"jsonrpc": "1.0", "method": "ping", "id": 1})
            assert resp["error"]["code"] == -32600
        asyncio.run(_go())

    def test_missing_method(self):
        server, _ = _make_init_server()
        async def _go():
            resp = await server.handle_message({"jsonrpc": "2.0", "id": 1})
            assert resp["error"]["code"] == -32600
        asyncio.run(_go())

    def test_unknown_method(self):
        server, _ = _make_init_server()
        async def _go():
            resp = await server.handle_message(_req("unknown/method"))
            assert resp["error"]["code"] == -32601
            assert "unknown/method" in resp["error"]["message"]
        asyncio.run(_go())

    def test_notification_unknown_method_no_response(self):
        server, _ = _make_init_server()
        async def _go():
            resp = await server.handle_message(_notification("unknown/method"))
            assert resp is None
        asyncio.run(_go())


# ── 3. Tool schemas ──────────────────────────────────────────────────────────


class TestToolSchemas:
    def test_nine_tools_registered(self):
        tools = _tool_defs(lambda: None)
        server = McpServer()
        server.register_tools(tools)
        async def _go():
            # Inline initialize (can't call _init_server_sync inside asyncio.run)
            await server.handle_message(_req("initialize", {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            }))
            await server.handle_message(_notification("notifications/initialized"))
            resp = await server.handle_message(_req("tools/list"))
            assert len(resp["result"]["tools"]) == 9
        asyncio.run(_go())

    def test_all_tool_names(self):
        tools = _tool_defs(lambda: None)
        expected = {
            "serial_send_command", "serial_get_state", "serial_get_logs",
            "serial_list_logs", "serial_reset", "serial_enter_uboot",
            "serial_wait_pattern", "serial_new_log", "serial_poll_logs",
        }
        actual = {t.name for t in tools}
        assert actual == expected

    def test_all_tools_have_description(self):
        for t in _tool_defs(lambda: None):
            assert t.description, f"{t.name} has empty description"
            assert len(t.description) > 10, f"{t.name} description too short"

    def test_all_tools_have_valid_schema(self):
        for t in _tool_defs(lambda: None):
            schema = t.input_schema
            assert schema["type"] == "object", f"{t.name} schema type != object"
            assert "properties" in schema, f"{t.name} missing properties"
            if "required" in schema:
                for req in schema["required"]:
                    assert req in schema["properties"], (
                        f"{t.name}: required '{req}' not in properties"
                    )

    def test_send_command_requires_command(self):
        tools = _tool_defs(lambda: None)
        t = next(t for t in tools if t.name == "serial_send_command")
        assert "command" in t.input_schema["required"]
        assert t.input_schema["properties"]["command"]["type"] == "string"

    def test_wait_pattern_requires_pattern(self):
        tools = _tool_defs(lambda: None)
        t = next(t for t in tools if t.name == "serial_wait_pattern")
        assert "pattern" in t.input_schema["required"]

    def test_no_param_tools_have_empty_properties(self):
        tools = _tool_defs(lambda: None)
        for name in ("serial_get_state", "serial_list_logs",
                      "serial_enter_uboot", "serial_new_log"):
            t = next(t for t in tools if t.name == name)
            assert t.input_schema["properties"] == {}, f"{name} should have no properties"

    def test_optional_params_have_defaults(self):
        tools = _tool_defs(lambda: None)
        t = next(t for t in tools if t.name == "serial_send_command")
        assert t.input_schema["properties"]["timeout"]["default"] == 90
        t = next(t for t in tools if t.name == "serial_wait_pattern")
        assert t.input_schema["properties"]["timeout"]["default"] == 60


# ── 4. Tool dispatch with MockEngine ─────────────────────────────────────────


class TestToolDispatch:
    def _call_tool(self, server: McpServer, name: str, args: dict | None = None, req_id=1):
        async def _go():
            return await server.handle_message(_req("tools/call", {
                "name": name, "arguments": args or {},
            }, req_id=req_id))
        return asyncio.run(_go())

    def test_get_state(self):
        server, mock = _make_init_server()
        resp = self._call_tool(server, "serial_get_state")
        result = json.loads(resp["result"]["content"][0]["text"])
        assert result["state"] == "active"
        assert result["login_configured"] is True

    def test_send_command(self):
        server, mock = _make_init_server()
        resp = self._call_tool(server, "serial_send_command", {"command": "uname -a"})
        result = json.loads(resp["result"]["content"][0]["text"])
        assert result["output"] == "mock: uname -a"
        assert result["exit_code"] == 0

    def test_send_command_with_timeout(self):
        server, mock = _make_init_server()
        resp = self._call_tool(server, "serial_send_command",
                                {"command": "sleep 100", "timeout": 5})
        result = json.loads(resp["result"]["content"][0]["text"])
        assert "output" in result

    def test_get_logs_default(self):
        server, mock = _make_init_server()
        resp = self._call_tool(server, "serial_get_logs")
        result = json.loads(resp["result"]["content"][0]["text"])
        assert "line1" in result["content"]
        mock.logs.read_log.assert_called_once_with(
            archive_index=0, lines=50, pattern=None,
        )

    def test_get_logs_with_filter(self):
        server, mock = _make_init_server()
        mock.logs.read_log.return_value = {
            "content": "ERROR: something broke",
            "filename": "boot-005.log",
            "total_lines": 100,
            "filtered_lines": 1,
        }
        resp = self._call_tool(server, "serial_get_logs",
                                {"lines": 10, "pattern": "ERROR", "archive": 1})
        result = json.loads(resp["result"]["content"][0]["text"])
        assert "ERROR" in result["content"]
        mock.logs.read_log.assert_called_once_with(
            archive_index=1, lines=10, pattern="ERROR",
        )

    def test_list_logs(self):
        server, mock = _make_init_server()
        resp = self._call_tool(server, "serial_list_logs")
        result = json.loads(resp["result"]["content"][0]["text"])
        assert len(result["archives"]) == 1
        assert result["archives"][0]["filename"] == "boot-005.log"

    def test_reset(self):
        server, mock = _make_init_server()
        resp = self._call_tool(server, "serial_reset")
        result = json.loads(resp["result"]["content"][0]["text"])
        assert result["success"] is True
        assert result["new_boot_number"] == 6

    def test_enter_uboot(self):
        server, mock = _make_init_server()
        resp = self._call_tool(server, "serial_enter_uboot")
        result = json.loads(resp["result"]["content"][0]["text"])
        assert result["success"] is True
        assert result["state_after"] == "uboot"

    def test_wait_pattern(self):
        server, mock = _make_init_server()
        resp = self._call_tool(server, "serial_wait_pattern",
                                {"pattern": "login:", "timeout": 30})
        result = json.loads(resp["result"]["content"][0]["text"])
        assert result["matched"] is True
        assert "login:" in result["matched_line"]

    def test_new_log(self):
        server, mock = _make_init_server()
        mock.logs.current_path = "/tmp/boot-006.log"
        resp = self._call_tool(server, "serial_new_log")
        result = json.loads(resp["result"]["content"][0]["text"])
        assert result["success"] is True
        mock.logs.rotate.assert_called_once()
        mock.detector.reset_cycle.assert_called_once()

    def test_unknown_tool_returns_error(self):
        server, _ = _make_init_server()
        resp = self._call_tool(server, "serial_nonexistent")
        assert "error" in resp
        assert resp["error"]["code"] == -32602

    def test_tool_with_no_arguments_field(self):
        """Tools should work when 'arguments' key is omitted entirely."""
        server, mock = _make_init_server()
        async def _go():
            return await server.handle_message(_req("tools/call", {
                "name": "serial_get_state",
            }))
        resp = asyncio.run(_go())
        result = json.loads(resp["result"]["content"][0]["text"])
        assert result["state"] == "active"


# ── 5. Engine not connected ──────────────────────────────────────────────────


class TestDisconnected:
    def test_all_tools_return_error_when_engine_none(self):
        server = McpServer()
        server.register_tools(_tool_defs(lambda: None))
        _init_server_sync(server)

        tool_args = {
            "serial_get_state": {},
            "serial_send_command": {"command": "echo hi"},
            "serial_get_logs": {},
            "serial_list_logs": {},
            "serial_reset": {},
            "serial_enter_uboot": {},
            "serial_new_log": {},
        }
        for name, args in tool_args.items():
            async def _go():
                return await server.handle_message(_req("tools/call", {
                    "name": name, "arguments": args,
                }))
            resp = asyncio.run(_go())
            result = json.loads(resp["result"]["content"][0]["text"])
            assert "error" in result, f"{name} should return error when disconnected"
            assert "not connected" in result["error"].lower()


# ── 6. Tool handler exceptions ────────────────────────────────────────────────


class TestToolExceptions:
    def test_handler_exception_returns_error(self):
        server = McpServer()
        async def bad_handler(args):
            raise RuntimeError("boom")
        server.register_tools([ToolDef(
            name="bad_tool", description="Always fails",
            input_schema={"type": "object", "properties": {}},
            handler=bad_handler, is_async=True,
        )])
        _init_server_sync(server)
        async def _go():
            return await server.handle_message(_req("tools/call", {
                "name": "bad_tool", "arguments": {},
            }))
        resp = asyncio.run(_go())
        assert "error" in resp
        assert resp["error"]["code"] == -32603
        assert "boom" in resp["error"]["message"]

    def test_sync_handler_works(self):
        server = McpServer()
        def sync_handler(args):
            return {"sync": True}
        server.register_tools([ToolDef(
            name="sync_tool", description="Sync tool",
            input_schema={"type": "object", "properties": {}},
            handler=sync_handler, is_async=False,
        )])
        _init_server_sync(server)
        async def _go():
            return await server.handle_message(_req("tools/call", {
                "name": "sync_tool", "arguments": {},
            }))
        resp = asyncio.run(_go())
        result = json.loads(resp["result"]["content"][0]["text"])
        assert result["sync"] is True


# ── 7. Full protocol flow ────────────────────────────────────────────────────


class TestFullFlow:
    def test_complete_lifecycle(self):
        server, mock = _make_server_with_mock()

        async def _go():
            # 1. Initialize
            resp = await server.handle_message(_req("initialize", {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "1.0"},
            }, req_id=1))
            assert resp["id"] == 1
            assert resp["result"]["protocolVersion"] == "2024-11-05"

            # 2. Initialized notification
            resp = await server.handle_message(_notification("notifications/initialized"))
            assert resp is None

            # 3. List tools
            resp = await server.handle_message(_req("tools/list", req_id=2))
            assert resp["id"] == 2
            tools = resp["result"]["tools"]
            assert len(tools) == 9
            names = {t["name"] for t in tools}
            assert "serial_get_state" in names

            # 4. Call a tool
            resp = await server.handle_message(_req("tools/call", {
                "name": "serial_send_command",
                "arguments": {"command": "cat /proc/version"},
            }, req_id=3))
            assert resp["id"] == 3
            result = json.loads(resp["result"]["content"][0]["text"])
            assert "mock:" in result["output"]

            # 5. Ping
            resp = await server.handle_message(_req("ping", req_id=4))
            assert resp["id"] == 4
            assert resp["result"] == {}

        asyncio.run(_go())

    def test_reinitialize_is_idempotent(self):
        server = McpServer()
        _init_server_sync(server)
        async def _go():
            resp = await server.handle_message(_req("initialize", {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            }))
            assert "result" in resp
            assert resp["result"]["protocolVersion"] == "2024-11-05"
        asyncio.run(_go())

    def test_multiple_tool_calls_in_sequence(self):
        server, mock = _make_init_server()
        async def _go():
            for i in range(5):
                resp = await server.handle_message(_req("tools/call", {
                    "name": "serial_get_state", "arguments": {},
                }, req_id=10 + i))
                result = json.loads(resp["result"]["content"][0]["text"])
                assert result["state"] == "active"
        asyncio.run(_go())

    def test_tool_result_is_valid_json(self):
        """All tool results should serialize to valid JSON."""
        server, mock = _make_init_server()
        async def _go():
            for name in ("serial_get_state", "serial_list_logs", "serial_new_log"):
                mock.logs.current_path = "/tmp/test.log"
                resp = await server.handle_message(_req("tools/call", {
                    "name": name, "arguments": {},
                }))
                text = resp["result"]["content"][0]["text"]
                parsed = json.loads(text)  # must not raise
                assert isinstance(parsed, dict)
        asyncio.run(_go())
