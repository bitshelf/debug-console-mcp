#!/usr/bin/env python3
"""Log manager — per-power-cycle log rotation, stored in .dut-serial/logs/."""

import os
from datetime import datetime
from pathlib import Path


class LogManager:
    """一次上电→掉电 = 一个 boot-NNN.log。存于 {project}/.dut-serial/logs/"""

    def __init__(self, project_dir: Path, max_logs: int = 10, max_file_size_mb: int = 100,
                 dut_dir: str = ".dut-serial"):
        self._dut_dir = project_dir / dut_dir
        self._log_dir = self._dut_dir / "logs"
        self._max_logs = max_logs
        self._max_file_size = max_file_size_mb * 1024 * 1024
        self._fd: int | None = None
        self._current_path: Path | None = None
        self._boot_number = 0

    @property
    def current_path(self) -> Path | None:
        return self._current_path

    @property
    def boot_number(self) -> int:
        return self._boot_number

    def open_current(self):
        """打开当前周期日志。"""
        self._log_dir.mkdir(parents=True, exist_ok=True)

        # .boot_count 在 .dut-serial/ 根目录
        count_file = self._log_dir.parent / ".boot_count"
        if count_file.exists():
            self._boot_number = int(count_file.read_text().strip()) + 1
        else:
            self._boot_number = 1
        count_file.write_text(str(self._boot_number))

        ts = datetime.now().strftime("%Y%m%d_%H%M%S")
        fname = f"boot-{self._boot_number:03d}_{ts}.log"
        self._current_path = self._log_dir / fname
        self._fd = os.open(
            str(self._current_path), os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644
        )

        header = f"=== Boot #{self._boot_number} — {datetime.now().isoformat()} ===\n"
        os.write(self._fd, header.encode())

        # 更新符号链接
        link = self._log_dir / "serial.current.log"
        if link.exists() or link.is_symlink():
            link.unlink()
        link.symlink_to(fname)

    def write(self, data: bytes):
        """追加原始串口数据到当前日志。超过大小限制自动 rotate。"""
        if self._fd:
            os.write(self._fd, data)
            try:
                if self._current_path and self._current_path.stat().st_size > self._max_file_size:
                    self.rotate()
            except OSError:
                pass

    def rotate(self):
        """切割: fsync+close → 清理旧日志 → 打开新。"""
        if self._fd:
            os.fsync(self._fd)
            os.close(self._fd)
            self._fd = None
        logs = sorted(self._log_dir.glob("boot-*_*.log"))
        for old in logs[: -self._max_logs]:
            old.unlink()
        self.open_current()

    def close(self):
        if self._fd:
            os.close(self._fd)
            self._fd = None

    def list_archives(self) -> list[dict]:
        logs = sorted(self._log_dir.glob("boot-*_*.log"), reverse=True)
        return [
            {"filename": p.name, "size_bytes": p.stat().st_size, "path": str(p)}
            for p in logs
        ]

    def read_log(
        self, archive_index: int = 0, lines: int = 50, pattern: str | None = None
    ) -> dict:
        """读取指定归档日志。archive_index=0 是当前日志。"""
        logs = sorted(self._log_dir.glob("boot-*_*.log"), reverse=True)
        if archive_index < 0 or archive_index >= len(logs):
            return {"content": "", "filename": "", "total_lines": 0, "filtered_lines": 0}
        target = logs[archive_index]
        content = target.read_text(errors="replace")
        all_lines = content.splitlines()
        if pattern:
            import re
            pat = re.compile(pattern, re.IGNORECASE)
            filtered = [l for l in all_lines if pat.search(l)]
        else:
            filtered = all_lines
        if lines > 0:
            filtered = filtered[-lines:]
        return {
            "content": "\n".join(filtered),
            "filename": target.name,
            "total_lines": len(all_lines),
            "filtered_lines": len(filtered),
        }
