#!/usr/bin/env python3
"""Tests for BootStageDetector — stage detection, crash detection, watchers."""

import re

import pytest
from boot_detector import BootStageDetector, BOOT_STAGES, CRASH_PATTERNS


class TestBootDetector:
    @pytest.fixture
    def bd(self):
        det = BootStageDetector()
        det.events = []
        det.on_boot_start = lambda: det.events.append("boot_start")
        det.on_autoboot = lambda: det.events.append("autoboot")
        det.on_login_prompt = lambda: det.events.append("login")
        det.on_password_prompt = lambda: det.events.append("password")
        det.on_crash = lambda t, l: det.events.append(f"crash:{t}")
        det.on_stage = lambda s: det.events.append(s)
        det.on_activity = lambda: det.events.append("activity")
        return det

    def test_spl_detection(self, bd):
        bd.feed(b"U-Boot SPL 2024.01\n")
        assert "boot_start" in bd.events
        assert "spl" in bd.events

    def test_autoboot_detection(self, bd):
        bd.feed(b"Hit any key to stop autoboot: 3\n")
        assert "autoboot" in bd.events

    def test_login_detection(self, bd):
        bd.feed(b"rockchip login: \n")
        assert "login" in bd.events

    def test_shell_prompt_detection(self, bd):
        bd.feed(b"root@target:~# \n")
        assert "shell" in bd.events

    def test_kernel_panic_detection(self, bd):
        bd.feed(b"Kernel panic - not syncing: Attempted to kill init!\n")
        assert any(e.startswith("crash:") for e in bd.events)
        assert any("panic" in e for e in bd.events)

    def test_bug_detection(self, bd):
        bd.feed(b"BUG: unable to handle kernel NULL pointer dereference\n")
        assert any("crash:" in e for e in bd.events)

    def test_oops_detection(self, bd):
        bd.feed(b"Oops: 0000 [#1] PREEMPT SMP\n")
        assert any("crash:" in e for e in bd.events)

    def test_watcher_callback(self, bd):
        matched = []
        pat = re.compile(rb"Linux version")
        bd.add_watcher(pat, lambda line: matched.append(line))
        bd.feed(b"Linux version 6.1.0\n")
        assert len(matched) == 1

    def test_spl_only_once_per_cycle(self, bd):
        bd.feed(b"U-Boot SPL 2024.01\n")
        bd.feed(b"U-Boot SPL 2024.01\n")  # 第二次不应该触发
        assert bd.events.count("boot_start") == 1

    def test_reset_cycle_allows_spl_again(self, bd):
        bd.feed(b"U-Boot SPL 2024.01\n")
        assert bd.events.count("boot_start") == 1
        bd.reset_cycle()
        bd.feed(b"U-Boot SPL 2024.01\n")
        assert bd.events.count("boot_start") == 2
