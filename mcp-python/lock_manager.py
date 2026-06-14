#!/usr/bin/env python3
"""Host:port mutual exclusion lock — O_EXCL atomic creation, zombie cleanup."""

import hashlib
import os
from datetime import datetime
from pathlib import Path


def acquire_lock(host: str, port: int, lock_dir: str = "/tmp/embedded-debug/locks") -> int | None:
    """尝试获取 host:port 的锁。返回 None=成功, 返回 int=冲突 PID。"""
    lock_key = hashlib.md5(f"{host}:{port}".encode()).hexdigest()[:8]
    lock_path = Path(lock_dir) / f"{lock_key}.lock"
    Path(lock_dir).mkdir(parents=True, exist_ok=True)

    # 检查已有锁
    if lock_path.exists():
        try:
            existing_pid = int(lock_path.read_text().split("\n")[0])
            os.kill(existing_pid, 0)
            return existing_pid  # 进程存活 → 冲突
        except (ValueError, OSError, ProcessLookupError):
            lock_path.unlink()  # 僵尸锁 → 清理

    # O_EXCL 原子创建
    try:
        fd = os.open(str(lock_path), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
        content = f"{os.getpid()}\n{host}:{port}\n{datetime.now().isoformat()}"
        os.write(fd, content.encode())
        os.close(fd)
        return None
    except FileExistsError:
        # 竞态: 另一个 Server 抢先创建
        try:
            existing_pid = int(lock_path.read_text().split("\n")[0])
            os.kill(existing_pid, 0)
            return existing_pid
        except (ValueError, OSError, ProcessLookupError):
            lock_path.unlink()
            return acquire_lock(host, port)  # 递归重试一次


def release_lock(host: str, port: int, lock_dir: str = "/tmp/embedded-debug/locks"):
    """释放 host:port 锁。"""
    lock_key = hashlib.md5(f"{host}:{port}".encode()).hexdigest()[:8]
    lock_path = Path(lock_dir) / f"{lock_key}.lock"
    if lock_path.exists():
        lock_path.unlink()
