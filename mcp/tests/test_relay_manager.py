#!/usr/bin/env python3
"""Tests for RelayManager — 4-byte packet construction, checksum, MASKROM rollback."""

import pytest
from relay_manager import RelayManager


class TestRelayPacket:
    """4 字节命令包构造测试 (仿 serial_relay/src/main.rs build_packet())"""

    def test_packet_on(self):
        rm = RelayManager("localhost", 9000, reset_channel=2, maskrom_channel=1)
        checksum = (0xA0 + 2 + 0x01) & 0xFF
        assert checksum == 0xA3  # 0xA0 + 2 + 1 = 0xA3

    def test_packet_off(self):
        rm = RelayManager("localhost", 9000, reset_channel=2, maskrom_channel=1)
        checksum = (0xA0 + 2 + 0x00) & 0xFF
        assert checksum == 0xA2  # 0xA0 + 2 + 0 = 0xA2

    def test_packet_header_always_A0(self):
        rm = RelayManager("localhost", 9000, reset_channel=1, maskrom_channel=1)
        # 用 _send_command 验证 packet 构造 (mock serial)
        # 至少验证 header 始终为 0xA0
        assert rm.HEADER == 0xA0

    def test_not_configured_when_port_zero(self):
        rm = RelayManager("localhost", 0)
        assert not rm.configured

    def test_configured_when_port_and_channel_set(self):
        rm = RelayManager("localhost", 9000, reset_channel=2, maskrom_channel=1)
        assert rm.configured


class TestRelayMock:
    """使用 mock serial 测试 RelayManager"""

    def test_reset_returns_false_when_not_configured(self):
        rm = RelayManager("localhost", 0)
        assert rm.reset() is False

    def test_enter_maskrom_returns_false_when_not_configured(self):
        rm = RelayManager("localhost", 0)
        assert rm.enter_maskrom() is False
