#!/usr/bin/env python3
"""Command queue — serialized execution + marker-based response routing.

仿 labgrid UBootDriver._run() 的 marker echo 模式:
  echo '{marker[:4]}''{marker[4:]}'; {cmd}; echo "$?"; echo '{marker[:4]}''{marker[4:]}'
"""

import asyncio
import re
import time
from dataclasses import dataclass

from marker import gen_marker

# 仿 labgrid util/helper.py re_vt100 — 清理 ANSI escape codes
RE_VT100 = re.compile(rb"(\x1b\[|\x9b)[0-?]*[ -/]*[@-~]|\x1b[>=]|\x1b[()][A-Z0-9]")


def _strip_ansi(data: bytes) -> bytes:
    """Strip ANSI/VT100 escape codes from serial data — 仿 labgrid re_vt100.sub()."""
    return RE_VT100.sub(b"", data)


@dataclass
class PendingCommand:
    command: str
    marker: str
    timeout: float
    future: asyncio.Future
    begin_sent: bool = False
    _found_begin: bool = False  # P0-1: 显式追踪 "已找到 begin_marker" 状态
    buffer: bytes = b""
    sent_at: float = 0.0

    @property
    def begin_marker(self) -> bytes:
        return self.marker.encode()

    @property
    def end_marker(self) -> bytes:
        return self.marker.encode()  # 与 begin 相同 — labgrid 同模式, 在数据流中出现两次

    def resolve(self, output: str = "", timed_out: bool = False):
        if not self.future.done():
            self.future.set_result({
                "output": output,
                "exit_code": None,
                "timed_out": timed_out,
            })


class CommandQueue:
    """Serialized command execution. Only one command runs at a time."""

    def __init__(self):
        self._pending: asyncio.Queue[PendingCommand] = asyncio.Queue()
        self._current: PendingCommand | None = None
        self._write_fn = None
        self._default_timeout = 90  # 与 lava BOOTLOADER_DEFAULT_CMD_TIMEOUT 对齐

    def set_write_fn(self, fn):
        self._write_fn = fn

    def execute(self, command: str, timeout: float = 90.0) -> asyncio.Future:
        marker = gen_marker()
        loop = asyncio.get_running_loop()
        fut = loop.create_future()
        cmd = PendingCommand(command=command, marker=marker, timeout=timeout, future=fut)
        if self._current is None:
            self._current = cmd
            self._send_command(cmd)
        else:
            loop.call_soon_threadsafe(
                self._pending.put_nowait, cmd
            )
        return fut

    async def execute_async(self, command: str, timeout: float = 90.0) -> dict:
        return await self.execute(command, timeout)

    def feed_serial_data(self, data: bytes):
        """Scan serial data stream for begin/end markers.

        仿 labgrid _run() marker 提取 — 同一个 marker 字符串在串口输出中出现两次:
        第一次 = begin, 第二次 = end. 两者之间的内容 = 命令输出.
        """
        data = _strip_ansi(data)

        if self._current is None:
            return

        pc = self._current
        if not pc.begin_sent:
            return

        # 步骤 1: 还没找到 begin_marker
        if not pc._found_begin:
            idx_begin = data.find(pc.begin_marker)
            if idx_begin >= 0:
                pc._found_begin = True
                pc.buffer = data[idx_begin + len(pc.begin_marker):]
                # 继续到步骤 2 — end_marker 可能在同一个 chunk 里
            else:
                return  # begin_marker 还没出现, 等下一个 chunk

        # 步骤 2: 已找到 begin_marker, 扫描 buffer 中的 end_marker
        idx_end = pc.buffer.find(pc.end_marker)
        if idx_end >= 0:
            output = pc.buffer[:idx_end].decode(errors="replace").replace("\r", "").strip()
            lines = output.split("\n")
            exit_code = None
            for line in reversed(lines):
                stripped = line.strip()
                if stripped.isdigit() or (stripped.startswith("-") and stripped[1:].isdigit()):
                    exit_code = int(stripped)
                    break
            pc.resolve(output, timed_out=False)
            rest = pc.buffer[idx_end + len(pc.end_marker):]
            self._current = None
            self._dequeue_next()
            if rest:
                self.feed_serial_data(rest)
        elif time.monotonic() - pc.sent_at > pc.timeout:
            pc.resolve(pc.buffer.decode(errors="replace").strip(), timed_out=True)
            self._current = None
            self._dequeue_next()
        # else: 等待更多数据或超时

    def _send_command(self, pc: PendingCommand):
        """Write command with shell-quoted echo markers — 仿 labgrid marker pattern.
        用单引号包裹 marker 防止 shell 展开, "$?" 双引号保护。"""
        m = pc.marker
        # 仿 labgrid: echo '{m[:4]}''{m[4:]}'; {cmd}; echo "$?"; echo '{m[:4]}''{m[4:]}'
        line = (
            f"echo '{m[:4]}''{m[4:]}'; "
            f"{pc.command}; "
            f'echo "$?"; '
            f"echo '{m[:4]}''{m[4:]}'\n"
        )
        if self._write_fn:
            self._write_fn(line.encode())
            pc.sent_at = time.monotonic()
            pc.begin_sent = True

    def _dequeue_next(self):
        if self._current is not None:
            return
        try:
            self._current = self._pending.get_nowait()
            self._send_command(self._current)
        except asyncio.QueueEmpty:
            pass
