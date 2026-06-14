#!/usr/bin/env python3
"""Relay manager — pyserial 直连 ser2net 控制 CH340 继电器，4 字节协议。

来自 serial_relay/src/main.rs 协议:
  Packet: [0xA0, channel(1-4), opcode, checksum]
  checksum = (0xA0 + channel + opcode) & 0xFF
  Baud: 9600 8N1
"""

import time
import serial


class RelayManager:
    """直连 ser2net 控制继电器 — 无 SSH, 无 serial_relay 二进制。"""

    HEADER = 0xA0
    OP_ON = 0x01     # 闭合继电器 (拉低引脚)
    OP_OFF = 0x00    # 断开继电器 (释放引脚)
    OP_TOGGLE = 0x04
    OP_STATUS = 0x05  # 返回 1 byte: bit0 → ON/OFF

    def __init__(
        self,
        host: str,
        port: int,
        reset_channel: int = 0,
        maskrom_channel: int = 0,
    ):
        self._host = host
        self._port = port
        self._reset_ch = reset_channel
        self._maskrom_ch = maskrom_channel
        self._ser: serial.Serial | None = None

    @property
    def configured(self) -> bool:
        return self._port > 0 and self._reset_ch > 0 and self._reset_ch <= 4

    # ── 长连接管理 ──────────────────────────────────────────

    def _ensure_open(self):
        """Lazy init + 自动重连。

        ser2net 重启后本地 socket is_open 仍为 True，所以不能仅依赖 is_open
        判断连接状态。_send_command 在写失败时会调用 _force_reconnect() 重建。

        注意: serial_for_url('socket://...') 默认 do_not_open=False 会自动打开连接。
        此处使用 do_not_open=True + 手动 open() 确保状态完全可控。
        """
        if not self._ser or not self._ser.is_open:
            self._ser = serial.serial_for_url(
                f"socket://{self._host}:{self._port}",
                baudrate=9600,
                timeout=0.8,
                do_not_open=True,
            )
            self._ser.open()

    def _force_reconnect(self):
        """强制关闭并重建连接 — ser2net 重启后使用。"""
        try:
            if self._ser:
                self._ser.close()
        except Exception:
            pass
        self._ser = None
        self._ensure_open()

    def close(self):
        if self._ser and self._ser.is_open:
            self._ser.close()

    # ── 底层: 4 字节命令 ───────────────────────────────────

    def _send_command(self, channel: int, opcode: int) -> bytes:
        """通过长连接发送 4 字节命令包。

        写失败时强制重连并重试一次 — 处理 ser2net 重启导致的 stale socket。
        """
        checksum = (self.HEADER + channel + opcode) & 0xFF
        packet = bytes([self.HEADER, channel, opcode, checksum])
        for attempt in (1, 2):
            try:
                self._ensure_open()
                self._ser.reset_input_buffer()
                self._ser.write(packet)
                self._ser.flush()
                time.sleep(0.05)  # 仿 serial_relay main.rs 50ms
                if opcode == self.OP_STATUS:
                    return self._ser.read(16)
                return b""
            except (serial.SerialException, OSError, BrokenPipeError) as e:
                if attempt == 1:
                    self._force_reconnect()
                    continue
                raise

    def _channel_on(self, channel: int):
        self._send_command(channel, self.OP_ON)

    def _channel_off(self, channel: int):
        self._send_command(channel, self.OP_OFF)

    def _channel_status(self, channel: int) -> str:
        resp = self._send_command(channel, self.OP_STATUS)
        if resp:
            return "ON" if (len(resp) >= 3 and resp[2] & 0x01) else "OFF"
        return "unknown"

    # ── 目标板操作 ──────────────────────────────────────────

    def reset(self) -> bool:
        """脉冲复位: RESET=低 → 500ms → RESET=高。"""
        if not self.configured:
            return False
        try:
            self._channel_on(self._reset_ch)
            time.sleep(0.5)
            self._channel_off(self._reset_ch)
            return True
        except (serial.SerialException, OSError):
            return False

    def enter_maskrom(self) -> bool:
        """
        MASKROM 序列:
        1. MASKROM=低  2. RESET=低  3. RESET=高  4. MASKROM=高
        任何步骤失败 → 回滚释放所有引脚。
        """
        if not self.configured or self._maskrom_ch == 0:
            return False
        try:
            self._channel_on(self._maskrom_ch)
            time.sleep(1)
            self._channel_on(self._reset_ch)
            time.sleep(1)
            self._channel_off(self._reset_ch)
            time.sleep(1)
            self._channel_off(self._maskrom_ch)
            return True
        except (serial.SerialException, OSError):
            # 回滚: 确保所有引脚释放
            try:
                self._channel_off(self._reset_ch)
                self._channel_off(self._maskrom_ch)
            except Exception:
                pass
            return False
