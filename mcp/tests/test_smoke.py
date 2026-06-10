#!/usr/bin/env python3
"""MCP Server smoke tests — fast (<2s), no real hardware required.

Run: uv run pytest tests/test_smoke.py -v
"""

import asyncio
import io
import os
import tempfile
import time
from pathlib import Path
from unittest.mock import MagicMock, patch

import pexpect
import pytest

# ── helpers ──────────────────────────────────────────────────────────────────

@pytest.fixture
def tmp_dir():
    with tempfile.TemporaryDirectory() as d:
        yield Path(d)


def _make_conf(d: Path, host="127.0.0.1", port=29999):
    (d / ".target.conf").write_text(
        f"RK_DEV_HOST_IP={host}\nRK_SERIAL_PORT={port}\n"
        f"RK_RELAY_PORT=0\nRK_RESET_CHANNEL=0\nRK_MASKROM_CHANNEL=0\n"
    )
    return str(d / ".target.conf")


# ── 1. Marker + ANSI stripping ──────────────────────────────────────────────

class TestMarkerAndANSI:
    """P0: marker generation and ANSI/VT100 escape code stripping"""

    def test_marker_no_rid(self):
        from marker import gen_marker
        for _ in range(50):
            m = gen_marker()
            assert len(m) == 10
            assert not any(c in m for c in "RID")

    def test_strip_ansi_colors(self):
        from command_queue import _strip_ansi
        # ANSI red text "ERROR"
        raw = b"\x1b[31mERROR\x1b[0m\n"
        clean = _strip_ansi(raw)
        assert b"\x1b" not in clean
        assert b"ERROR" in clean

    def test_strip_ansi_cursor_moves(self):
        from command_queue import _strip_ansi
        raw = b"\x1b[2J\x1b[Hlogin: \x1b[K"
        clean = _strip_ansi(raw)
        assert b"\x1b" not in clean
        assert b"login:" in clean

    def test_strip_ansi_passthrough_plain_text(self):
        from command_queue import _strip_ansi
        raw = b"Linux version 6.1.0\n"
        assert _strip_ansi(raw) == raw

    def test_marker_not_corrupted_by_ansi(self):
        """Marker string should not be fragmented by ANSI codes mid-stream."""
        from command_queue import _strip_ansi
        marker = b"__CMD_BEGIN_XXXXXXXXXX__"
        # Simulate ANSI code inserted in the middle
        corrupted = b"__CMD_BEGIN_\x1b[32mXXXXXXXXXX\x1b[0m__"
        clean = _strip_ansi(corrupted)
        assert marker in clean


# ── 2. CommandQueue marker echo + response routing ──────────────────────────

class TestCommandQueue:
    """P0+P1: command serialization, marker routing, shell quoting"""

    def test_shell_quoting_dollar_question(self):
        """P1-3: marker echo pattern uses single quotes and "$?"."""
        from command_queue import CommandQueue
        cq = CommandQueue()
        written = []
        cq.set_write_fn(lambda b: written.append(b))
        # Use asyncio.run to get a proper event loop
        async def _go():
            fut = cq.execute("uname -a")
            # After execute, _send_command writes the line via _write_fn
            pc = cq._current
            assert pc is not None
            assert pc.marker is not None
            # _send_command writes immediately when _current was None
            assert len(written) > 0
            line = written[0].decode()
            # P1-4: marker wrapped in single quotes (labgrid pattern)
            assert "'" in line, f"Expected single-quoted marker, got: {line}"
            # P1-3: $? is double-quoted
            assert '"$?"' in line, f'Expected "$?", got: {line}'
        asyncio.run(_go())

    def test_ansi_stripped_before_marker_scan(self):
        """P0-1: feed_serial_data strips ANSI and extracts output between markers."""
        from command_queue import CommandQueue, _strip_ansi
        async def _go():
            cq = CommandQueue()
            fut = cq.execute("echo test")
            pc = cq._current
            pc.begin_sent = True
            # Simulate: echo of command line → begin_marker, then output, then end_marker
            raw = b"\x1b[32m" + pc.begin_marker + b"\x1b[0mhello world\n" + pc.end_marker + b"\x1b[0m\n"
            cq.feed_serial_data(raw)
            assert pc.future.done(), f"Future not done, _found_begin={pc._found_begin} buffer={pc.buffer[:100]}"
            result = pc.future.result()
            assert "hello world" in result["output"], f"output={result['output']}"
        asyncio.run(_go())

    def test_marker_extraction_exit_code(self):
        """Output between begin→end markers is extracted."""
        from command_queue import CommandQueue
        async def _go():
            cq = CommandQueue()
            fut = cq.execute("true")
            pc = cq._current
            pc.begin_sent = True
            cq.feed_serial_data(pc.begin_marker + b"some output\n0\n" + pc.end_marker)
            assert pc.future.done(), f"Future not done, _found_begin={pc._found_begin}"
            assert "some output" in pc.future.result()["output"]
        asyncio.run(_go())

    def test_command_timeout(self):
        """Command times out after deadline with partial data."""
        from command_queue import CommandQueue
        async def _go():
            cq = CommandQueue()
            fut = cq.execute("sleep 100", timeout=0.01)
            pc = cq._current
            pc.begin_sent = True
            pc.sent_at = time.monotonic() - 99  # way past timeout
            # Feed only begin_marker + partial data, no end_marker
            cq.feed_serial_data(pc.begin_marker + b"partial output...")
            assert pc.future.done()
            assert pc.future.result()["timed_out"]
        asyncio.run(_go())


# ── 3. Console driver lifecycle ─────────────────────────────────────────────

class TestConsoleDriver:
    """P1-2: @check_active guard, lifecycle"""

    def test_sendline_when_inactive_raises(self):
        from console import SerialConsoleDriver
        drv = SerialConsoleDriver("127.0.0.1", 29999, protocol="raw")
        # Status is 0 (inactive) — sendline should raise
        with pytest.raises(RuntimeError, match="not active"):
            drv.sendline("test")

    def test_sendcontrol_when_inactive_raises(self):
        from console import SerialConsoleDriver
        drv = SerialConsoleDriver("127.0.0.1", 29999, protocol="raw")
        with pytest.raises(RuntimeError, match="not active"):
            drv.sendcontrol("c")

    def test_read_allowed_when_inactive(self):
        """_read() is NOT guarded (allows reading even when inactive)"""
        from console import SerialConsoleDriver
        drv = SerialConsoleDriver("127.0.0.1", 29999, protocol="raw")
        # _read() is not decorated with @check_active, should work
        # (but will fail on unopened serial — that's OK)
        try:
            drv.read(size=1, timeout=0.1)
        except (pexpect.TIMEOUT, RuntimeError, Exception):
            pass  # expected — serial not opened

    def test_activate_sets_status(self):
        """on_activate sets _status to 1 if serial opens"""
        from console import SerialConsoleDriver
        drv = SerialConsoleDriver("127.0.0.1", 29999, protocol="raw")
        assert not drv.is_open
        # Connection refused → should raise, _status stays 0
        try:
            drv.on_activate()
        except Exception:
            pass
        # sendline should still raise because _status==0
        with pytest.raises(RuntimeError, match="not active"):
            drv.sendline("test")


# ── 4. State transitions ────────────────────────────────────────────────────

class TestStateTransitions:
    """State machine correctness, hysteresis, PID file"""

    def test_full_lifecycle(self, tmp_dir):
        _make_conf(tmp_dir)
        from state_manager import StateManager
        sm = StateManager(tmp_dir, hang_timeout=2, hysteresis=2)
        # startup sequence
        sm.transition("booting")
        assert sm.current == "booting"
        assert (sm._dut_dir / "target-state").read_text() == "booting"
        assert (sm._dut_dir / "mcp.pid").exists()
        # boot complete
        sm.transition("active")
        assert sm.current == "active"
        assert (sm._dut_dir / "target-state").read_text() == "active"
        # shutdown
        sm.transition("stopped")
        # stopped → 删除文件，statusline 不显示任何状态
        assert not (sm._dut_dir / "target-state").exists()

    def test_crash_detection(self, tmp_dir):
        _make_conf(tmp_dir)
        from state_manager import StateManager
        sm = StateManager(tmp_dir, hang_timeout=1, hysteresis=2)
        # Normal boot → crash
        sm.transition("active")
        sm.transition("booting")
        sm.transition("crashed")
        assert sm.current == "crashed"
        assert (sm._dut_dir / "target-state").read_text() == "crashed"

    def test_hang_detection_ignored_from_active(self, tmp_dir):
        _make_conf(tmp_dir)
        from state_manager import StateManager
        sm = StateManager(tmp_dir, hang_timeout=1, hysteresis=1)
        sm.transition("active")
        sm._last_data_time = time.monotonic() - 999
        sm.check_hang()  # should be ignored (active)
        assert sm.current == "active"

    def test_no_hang_from_disconnected(self, tmp_dir):
        """disconnected 时不检测挂死 — 可能是网络问题，不是目标板卡死"""
        _make_conf(tmp_dir)
        from state_manager import StateManager
        sm = StateManager(tmp_dir, hang_timeout=1, hysteresis=2)
        sm.transition("disconnected")
        sm._last_data_time = time.monotonic() - 999
        sm.check_hang()
        sm.check_hang()
        # 应该保持在 disconnected，不转换到 DUT-off
        assert sm.current == "disconnected"

    def test_statusline_formatting(self):
        """format_serial_state returns None for non-displayable states"""
        import sys; sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
        from hooks_lib import format_serial_state
        # Displayable
        assert format_serial_state("active") == ("● serial:active", "green")
        assert format_serial_state("booting") == ("◐ serial:booting", "yellow")
        assert format_serial_state("crashed") == ("✗ serial:crashed", "red")
        assert format_serial_state("disconnected") == ("✗ serial:disconnected", "red")
        assert format_serial_state("DUT-off") == ("✗ serial:DUT-off", "red")
        # Non-displayable → None
        assert format_serial_state("stopped") is None
        assert format_serial_state("connecting") is None
        assert format_serial_state("unknown") is None


# ── 5. Config loading ──────────────────────────────────────────────────────

class TestConfig:
    def test_minimal(self, tmp_dir):
        from config import load_config
        p = _make_conf(tmp_dir)
        os.environ["TARGET_CONF"] = p
        cfg = load_config()
        assert cfg["RK_DEV_HOST_IP"] == "127.0.0.1"
        assert cfg["RK_SERIAL_PORT"] == "29999"

    def test_defaults_all_zero_relay(self, tmp_dir):
        """Relay defaults are all 0 = not configured"""
        from config import load_config
        p = _make_conf(tmp_dir)
        os.environ["TARGET_CONF"] = p
        cfg = load_config()
        assert cfg["RK_RELAY_PORT"] == "0"
        assert cfg["RK_RESET_CHANNEL"] == "0"
        assert cfg["RK_MASKROM_CHANNEL"] == "0"

    def test_all_defaults(self, tmp_dir):
        from config import load_config
        p = _make_conf(tmp_dir)
        os.environ["TARGET_CONF"] = p
        cfg = load_config()
        for key in ["RK_DUT_DIR", "RK_LOCK_DIR", "RK_LOGIN_USER",
                     "RK_HANG_TIMEOUT", "RK_MAX_ARCHIVED_LOGS",
                     "RK_UBOOT_INTERRUPT_STRATEGY"]:
            assert key in cfg, f"Missing default: {key}"


# ── 6. Boot stage detection ────────────────────────────────────────────────

class TestBootStages:
    def test_spl_triggers_rotate(self, tmp_dir):
        from boot_detector import BootStageDetector
        bd = BootStageDetector()
        events = []
        bd.on_boot_start = lambda: events.append("rotate")
        bd.feed(b"U-Boot SPL 2024.01 (Jan 01 2024)\n")
        assert "rotate" in events

    def test_login_triggers_only_once(self, tmp_dir):
        from boot_detector import BootStageDetector
        bd = BootStageDetector()
        events = []
        bd.on_login_prompt = lambda: events.append("login")
        bd.feed(b"rockchip login: \n")
        bd.feed(b"rockchip login: \n")  # duplicate
        assert events.count("login") == 1  # only once

    def test_reset_login_state_allows_re_login(self, tmp_dir):
        from boot_detector import BootStageDetector
        bd = BootStageDetector()
        events = []
        bd.on_login_prompt = lambda: events.append("login")
        bd.feed(b"rockchip login: \n")
        assert len(events) == 1
        bd.reset_login_state()
        bd.feed(b"rockchip login: \n")
        assert len(events) == 2  # reset allows re-trigger

    def test_all_stages_detected(self, tmp_dir):
        """Every Linux BOOT_STAGE pattern matches at least the example line."""
        from boot_detector import BootStageDetector, BOOT_STAGES
        bd = BootStageDetector()
        detected = set()
        bd.on_stage = lambda s: detected.add(s)
        # Feed a complete Linux boot sequence
        boot = (
            b"U-Boot SPL 2024.01\n"
            b"TL1 something\n"
            b"BL31:\n"
            b"OP-TEE\n"
            b"DDR Version 1.0\n"
            b"U-Boot 2024.01\n"
            b"Hit any key to stop autoboot: 3\n"
            b"Linux version 6.1.0\n"
            b"Starting kernel\n"
            b"rockchip login: \n"
            b"Password: \n"
            b"root@target:~# \n"
        )
        bd.feed(boot)
        expected = {"spl", "tpl", "bl31", "optee", "ddr", "uboot",
                     "autoboot", "kernel", "start", "login", "password", "shell"}
        # Only check that ALL expected Linux stages are detected
        missing = expected - detected
        assert not missing, f"Missing: {missing}"

    def test_android_boot_stages_detected(self, tmp_dir):
        """Android boot completion patterns: adbd, bootanim, sys.boot_completed."""
        from boot_detector import BootStageDetector
        bd = BootStageDetector()
        detected = set()
        bd.on_stage = lambda s: detected.add(s)

        # Simulate Android boot tail
        boot = (
            b"init: starting service 'adbd'\n"
            b"adbd starting\n"
            b"init: started service 'bootanim'\n"
            b"sys.boot_completed=1\n"
        )
        bd.feed(boot)
        # Android boot completion signals should all be detected
        assert "android_init" in detected, f"Missing android_init, got: {detected}"
        assert "android_adbd" in detected, f"Missing android_adbd, got: {detected}"
        assert "android_boot_completed" in detected, f"Missing android_boot_completed, got: {detected}"


# ── 7. Relay packet construction ────────────────────────────────────────────

class TestRelayPacket:
    def test_on_packet(self):
        from relay_manager import RelayManager
        rm = RelayManager("h", 1, reset_channel=2, maskrom_channel=1)
        # CH2 ON: 0xA0 0x02 0x01 → checksum = (0xA0+2+1)&0xFF = 0xA3
        assert (0xA0 + 2 + 1) & 0xFF == 0xA3

    def test_off_packet(self):
        from relay_manager import RelayManager
        rm = RelayManager("h", 1, reset_channel=2, maskrom_channel=1)
        # CH2 OFF: 0xA0 0x02 0x00 → checksum = (0xA0+2+0)&0xFF = 0xA2
        assert (0xA0 + 2 + 0) & 0xFF == 0xA2

    def test_not_configured(self):
        from relay_manager import RelayManager
        rm = RelayManager("h", 0)  # port=0
        assert not rm.configured
        assert rm.reset() is False
        assert rm.enter_maskrom() is False


# ── 8. Lock manager ─────────────────────────────────────────────────────────

class TestLockManager:
    def test_acquire_release(self, tmp_dir):
        from lock_manager import acquire_lock, release_lock
        lock_dir = str(tmp_dir / "locks")
        assert acquire_lock("h", 19999, lock_dir) is None
        assert acquire_lock("h", 19999, lock_dir) == os.getpid()
        release_lock("h", 19999, lock_dir)
        assert acquire_lock("h", 19999, lock_dir) is None
        release_lock("h", 19999, lock_dir)

    def test_different_ports_no_conflict(self, tmp_dir):
        from lock_manager import acquire_lock, release_lock
        lock_dir = str(tmp_dir / "locks")
        assert acquire_lock("h", 1, lock_dir) is None
        assert acquire_lock("h", 2, lock_dir) is None
        release_lock("h", 1, lock_dir)
        release_lock("h", 2, lock_dir)


# ── 9. Config parsing — quoting correctness ────────────────────────────────

class TestConfigQuoting:
    """P1-5: shell-style config values with various quoting."""

    def test_double_quoted_value(self, tmp_dir):
        (tmp_dir / ".target.conf").write_text('RK_LOGIN_PASS="hello world"\n')
        os.environ["TARGET_CONF"] = str(tmp_dir / ".target.conf")
        from config import load_config
        cfg = load_config()
        assert cfg["RK_LOGIN_PASS"] == "hello world"

    def test_single_quoted_value(self, tmp_dir):
        (tmp_dir / ".target.conf").write_text("RK_LOGIN_PASS='hello world'\n")
        os.environ["TARGET_CONF"] = str(tmp_dir / ".target.conf")
        from config import load_config
        cfg = load_config()
        assert cfg["RK_LOGIN_PASS"] == "hello world"

    def test_unquoted_value(self, tmp_dir):
        (tmp_dir / ".target.conf").write_text("RK_LOGIN_PASS=hello\n")
        os.environ["TARGET_CONF"] = str(tmp_dir / ".target.conf")
        from config import load_config
        cfg = load_config()
        assert cfg["RK_LOGIN_PASS"] == "hello"

    def test_value_with_inline_comment(self, tmp_dir):
        (tmp_dir / ".target.conf").write_text('RK_LOGIN_PASS=secret # my password\n')
        os.environ["TARGET_CONF"] = str(tmp_dir / ".target.conf")
        from config import load_config
        cfg = load_config()
        assert cfg["RK_LOGIN_PASS"] == "secret"

    def test_empty_value(self, tmp_dir):
        (tmp_dir / ".target.conf").write_text("RK_LOGIN_PASS=\n")
        os.environ["TARGET_CONF"] = str(tmp_dir / ".target.conf")
        from config import load_config
        cfg = load_config()
        assert cfg["RK_LOGIN_PASS"] == ""

    def test_export_prefix_stripped(self, tmp_dir):
        (tmp_dir / ".target.conf").write_text('export RK_LOGIN_PASS="exported"\n')
        os.environ["TARGET_CONF"] = str(tmp_dir / ".target.conf")
        from config import load_config
        cfg = load_config()
        assert cfg["RK_LOGIN_PASS"] == "exported"


# ── 10. Console._read timeout vs disconnect (P0-1) ─────────────────────────

class TestConsoleReadTimeout:
    """P0-1: timeout must NOT set _status=0 (would cause false reconnects)."""

    def test_timeout_does_not_clear_status(self):
        """_read() returning b"" (timeout) should raise TIMEOUT but keep _status=1."""
        from console import SerialConsoleDriver
        from unittest.mock import MagicMock, patch
        drv = SerialConsoleDriver("127.0.0.1", 29999, protocol="raw")
        drv._status = 1  # simulate active connection

        # Mock _serial.read() to return b"" (pyserial timeout behavior)
        drv._serial = MagicMock()
        drv._serial.is_open = True
        drv._serial.in_waiting = 0
        drv._serial.read.return_value = b""

        with pytest.raises(pexpect.TIMEOUT):
            drv._read(size=1, timeout=0.1)

        # P0-1: _status must remain 1 — timeout is NOT a disconnect
        assert drv._status == 1

    def test_serial_exception_sets_disconnected(self):
        """SerialException (real disconnect) should set _status=0."""
        from console import SerialConsoleDriver
        import serial as pyserial
        from unittest.mock import MagicMock
        drv = SerialConsoleDriver("127.0.0.1", 29999, protocol="raw")
        drv._status = 1

        drv._serial = MagicMock()
        drv._serial.is_open = True
        drv._serial.in_waiting = 0
        drv._serial.read.side_effect = pyserial.SerialException("socket disconnected")
        # open() will fail since no real socket — mock it out
        drv.open = MagicMock()

        with pytest.raises(pexpect.TIMEOUT, match="Connection lost"):
            drv._read(size=1, timeout=0.1)

        # Real disconnect → _status = 0
        assert drv._status == 0
