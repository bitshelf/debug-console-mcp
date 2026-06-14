#!/usr/bin/env python3
"""Console driver — 仿 labgrid SerialDriver + ConsoleExpectMixin 三层结构."""

import time
from abc import ABC, abstractmethod

import pexpect
import serial
import serial.rfc2217

from ptx_expect import PtxExpect


class ConsoleProtocol(ABC):
    """仿 labgrid protocol/consoleprotocol.py"""

    @abstractmethod
    def _read(self, size: int = 1, timeout: float = 0.0,
              max_size: int | None = None) -> bytes:
        ...

    @abstractmethod
    def _write(self, data: bytes) -> int:
        ...


class ConsoleExpectMixin:
    """仿 labgrid driver/consoleexpectmixin.py.
    依赖: self._read(), self._write(), self.logger, self.txdelay, self.txchunk.
    """

    txdelay: float
    txchunk: int

    def __init_console_expect__(self):
        self._expect = PtxExpect(self)

    # 仿 labgrid Driver.check_active — 非 active 时拒绝操作
    @staticmethod
    def check_active(method):
        def wrapper(self, *args, **kwargs):
            if not getattr(self, '_status', 0):
                raise RuntimeError(f"{self.__class__.__name__}.{method.__name__}: not active")
            return method(self, *args, **kwargs)
        return wrapper

    def read(self, size=1, timeout=0.0, max_size=None) -> bytes:
        return self._read(size=size, timeout=timeout, max_size=max_size)

    def write(self, data: bytes) -> int:
        if self.txdelay:
            count = 0
            for i in range(0, len(data), self.txchunk):
                time.sleep(self.txdelay)
                count += self._write(data[i : i + self.txchunk])
            return count
        return self._write(data)

    @check_active
    def sendline(self, line: str):
        self._expect.sendline(line)

    @check_active
    def sendcontrol(self, char: str):
        self._expect.sendcontrol(char)

    @check_active
    def expect(self, pattern, timeout=-1):
        return self._expect.expect(pattern, timeout=timeout)

    def settle(self, quiet_time: float, timeout: float = 120.0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                self.read(timeout=quiet_time)
            except pexpect.TIMEOUT:
                return True
        return False


class SerialConsoleDriver(ConsoleExpectMixin, ConsoleProtocol):
    """pyserial 传输实现 — 仿 labgrid SerialDriver."""

    def __init__(self, host: str, port: int, protocol: str = "raw",
                 baudrate: int = 1500000, txdelay: float = 0.0, txchunk: int = 1,
                 logger=None):
        self.host = host
        self.port = port
        self.protocol = protocol
        self.baudrate = baudrate
        self.txdelay = txdelay
        self.txchunk = txchunk
        self.logger = logger
        self._status = 0

        # 仿 labgrid SerialDriver.__attrs_post_init__()
        if protocol == "rfc2217":
            self._serial = serial.rfc2217.Serial()
        else:  # "raw"
            self._serial = serial.serial_for_url("socket://", do_not_open=True)

        self.__init_console_expect__()

    @property
    def is_open(self) -> bool:
        return self._status == 1

    # ── Driver lifecycle ───────────────────────────────────

    def on_activate(self):
        if self.protocol == "rfc2217":
            self._serial.port = (
                f"rfc2217://{self.host}:{self.port}?ign_set_control"
            )
        else:
            self._serial.port = f"socket://{self.host}:{self.port}/"
        self._serial.baudrate = self.baudrate
        self.open()

    def on_deactivate(self):
        self.close()

    def open(self):
        if not self._status:
            # 关闭旧 socket — 防止 fd 泄漏 (watchdog 重连路径)
            try:
                if self._serial.is_open:
                    self._serial.close()
            except Exception:
                pass
            self._serial.timeout = 5  # prevent indefinite block
            self._serial.open()
            self._status = 1

    def close(self):
        if self._status:
            self._serial.close()
            self._status = 0

    # ── 底层 I/O ──────────────────────────────────────────

    def _read(self, size: int = 1, timeout: float = 0.0,
              max_size: int | None = None) -> bytes:
        """仿 labgrid SerialDriver._read()

        区分两种"空读"场景:
        - pyserial socket transport: TCP 断开 → recv() 返回空 → 抛 SerialException
        - pyserial socket transport: 超时 → select 无就绪 → read() 返回 b""

        因此 b"" 返回 = 正常超时, 不改 _status; 由 _read_loop 的 streak 机制处理重连。
        """
        reading = max(size, self._serial.in_waiting)
        if max_size is not None:
            reading = min(reading, max_size)
        self._serial.timeout = timeout
        try:
            res = self._serial.read(reading)
        except (serial.SerialException, OSError) as e:
            self._status = 0
            if self.logger:
                self.logger.warning(f"Serial read error, reconnecting: {e}")
            self.open()
            raise pexpect.TIMEOUT(f"Connection lost, reconnected: {e}")
        if not res:
            # 正常超时 — 不改 _status, 让 _read_loop 的 streak 机制判断是否重连
            raise pexpect.TIMEOUT(
                f"Timeout of {timeout:.2f}s exceeded"
            )
        return res

    def _write(self, data: bytes) -> int:
        """仿 labgrid SerialDriver._write()"""
        return self._serial.write(data)
