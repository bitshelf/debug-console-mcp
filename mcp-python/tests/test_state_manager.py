#!/usr/bin/env python3
"""Tests for StateManager — hysteresis, atomic write, state filtering."""

import os
import tempfile
import time
from pathlib import Path

import pytest
from state_manager import StateManager


class TestStateManager:
    @pytest.fixture
    def tmp_dir(self):
        with tempfile.TemporaryDirectory() as d:
            yield Path(d)

    @pytest.fixture
    def sm(self, tmp_dir):
        """Create a .target.conf so project_dir detection works"""
        (tmp_dir / ".target.conf").write_text("RK_DEV_HOST_IP=1.2.3.4\nRK_SERIAL_PORT=2000\n")
        return StateManager(project_dir=tmp_dir, hang_timeout=2, hysteresis=2)

    def test_initial_state_is_stopped(self, sm):
        assert sm.current == "stopped"
        assert sm.external_state is None

    def test_transition_to_active_writes_file(self, sm):
        sm.transition("active")
        assert sm.current == "active"
        assert sm.external_state == "active"
        state_file = sm._dut_dir / "target-state"
        assert state_file.exists()
        assert state_file.read_text() == "active"

    def test_stopped_deletes_state_file(self, sm):
        """MCP Server 关闭 → 删除状态文件 → statusline 不显示"""
        sm.transition("active")
        sf = sm._dut_dir / "target-state"
        assert sf.exists()
        assert sf.read_text() == "active"

        sm.transition("stopped")
        assert sm.current == "stopped"
        # stopped → 删除文件
        assert not sf.exists(), "state file should be deleted when stopped"

    def test_connecting_does_not_write_file(self, sm):
        sm.transition("connecting")
        assert sm.current == "connecting"
        assert sm.external_state is None  # 不在 EXTERNAL_STATES 中
        assert not (sm._dut_dir / "target-state").exists()

    def test_hang_detection_from_booting(self, sm):
        sm.transition("booting")
        # 模拟超时无活动
        sm._last_data_time = time.monotonic() - 10
        sm.check_hang()  # count=1
        sm.check_hang()  # count=2 → DUT-off
        assert sm.current == "DUT-off"

    def test_no_hang_detection_from_active(self, sm):
        sm.transition("active")
        sm._last_data_time = time.monotonic() - 10
        for _ in range(5):
            sm.check_hang()
        assert sm.current == "active"  # active 不检测挂死

    def test_hang_hysteresis_resets_on_activity(self, sm):
        sm.transition("booting")
        # 先积累一次超时
        sm._last_data_time = time.monotonic() - 10
        sm.check_hang()  # count=1
        assert sm._hang_count == 1
        # 活动 → 重置
        sm.on_activity()
        assert sm._hang_count == 0
        # 再次超时: 需要等 hang_timeout 过了
        sm._last_data_time = time.monotonic() - 10
        sm.check_hang()  # count=1
        sm.check_hang()  # count=2 → DUT-off
        assert sm.current == "DUT-off"

    def test_external_states_filter(self, sm):
        """验证 MCP API 不暴露 stopped/connecting"""
        for state in ["stopped", "connecting"]:
            sm.transition(state)
            assert sm.external_state is None
        for state in ["active", "booting", "uboot", "crashed", "DUT-off", "disconnected"]:
            sm.transition(state)
            assert sm.external_state == state
