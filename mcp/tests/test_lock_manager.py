#!/usr/bin/env python3
"""Tests for lock_manager — O_EXCL atomic creation, zombie cleanup."""

import os
import tempfile
from pathlib import Path

import pytest
from lock_manager import acquire_lock, release_lock


class TestLockManager:
    @pytest.fixture
    def lock_dir(self):
        with tempfile.TemporaryDirectory() as d:
            yield d

    def test_acquire_release(self, lock_dir):
        pid = acquire_lock("testhost", 9999, lock_dir)
        assert pid is None  # 成功获取
        # 验证 lock 文件存在
        import hashlib
        lock_key = hashlib.md5("testhost:9999".encode()).hexdigest()[:8]
        lock_path = Path(lock_dir) / f"{lock_key}.lock"
        assert lock_path.exists()

        release_lock("testhost", 9999, lock_dir)
        assert not lock_path.exists()

    def test_conflict_detection(self, lock_dir):
        # 先占锁
        pid1 = acquire_lock("testhost", 9998, lock_dir)
        assert pid1 is None
        # 第二次尝试 → 应该返回冲突 PID
        pid2 = acquire_lock("testhost", 9998, lock_dir)
        assert pid2 == os.getpid()  # 返回自己的 PID

        release_lock("testhost", 9998, lock_dir)

    def test_different_ports_no_conflict(self, lock_dir):
        pid1 = acquire_lock("testhost", 9001, lock_dir)
        pid2 = acquire_lock("testhost", 9002, lock_dir)
        assert pid1 is None
        assert pid2 is None  # 不同端口, 不冲突

        release_lock("testhost", 9001, lock_dir)
        release_lock("testhost", 9002, lock_dir)
