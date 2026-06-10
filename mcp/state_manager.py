#!/usr/bin/env python3
"""State manager — 滞后防抖 + 原子写状态文件 + 状态空间约定。

状态语义 (statusline 显示):
  - booting:      启动中 (SPL 检测到，正在启动)
  - active:       启动完成，shell 就绪，可执行命令
  - uboot:        U-Boot 交互模式
  - crashed:      内核崩溃 (panic/BUG/Oops)
  - disconnected: 连不上 dev host (ser2net 不可达)
  - DUT-off:      目标卡死，长时间无输出

内部状态 (不直接显示):
  - stopped:      MCP Server 未运行
  - connecting:   正在建立串口连接 (短暂过渡)
"""

import logging
import time
from pathlib import Path

logger = logging.getLogger("embedded-debug")


class StateManager:
    """Atomically writes target-state file with hysteresis for hang detection."""

    # 外部可见的状态 (写入 target-state 文件，statusline 读取显示)
    EXTERNAL_STATES = {"active", "booting", "uboot", "crashed", "DUT-off", "disconnected"}

    def __init__(self, project_dir: Path, hang_timeout: int = 60, hysteresis: int = 3,
                 dut_dir: str = ".dut-serial"):
        self._dut_dir = project_dir / dut_dir
        self._state_file = self._dut_dir / "target-state"
        self._pid_file = self._dut_dir / "mcp.pid"
        self._hang_timeout = hang_timeout
        self._hysteresis = hysteresis
        self._current = "stopped"
        self._hang_count = 0
        self._last_data_time = time.monotonic()
        self._dut_dir.mkdir(parents=True, exist_ok=True)
        # 写入 PID 文件 (statusline 用来做存活检测)
        import os as _os
        self._pid_file.write_text(str(_os.getpid()))

    @property
    def current(self) -> str:
        return self._current

    @property
    def external_state(self) -> str | None:
        """MCP API 返回的状态。stopped/connecting → None。"""
        if self._current in self.EXTERNAL_STATES:
            return self._current
        return None

    def transition(self, new: str):
        """状态切换。stopped → 删文件; connecting → 不写。"""
        if new == self._current:
            return
        logger.info(f"StateManager: {self._current} → {new}")
        self._current = new
        if new == "stopped":
            # MCP Server 关闭 → 删除状态文件和 PID 文件
            # statusline 检测到文件不存在 → 不显示任何状态
            self._delete_state_file()
            self._delete_pid_file()
            logger.info("StateManager: deleted state+pid files (server stopped)")
        elif new == "connecting":
            pass  # 不写文件, 避免 statusline 闪烁
        else:
            self._atomic_write(new)

    def on_activity(self):
        """每次收到串口数据时调用。重置挂死计数器。"""
        self._last_data_time = time.monotonic()
        self._hang_count = 0

    def check_hang(self):
        """仅从 booting 检测挂死 — 目标板长时间无输出=启动失败。
        disconnected 时不检测: 可能是 MCP-TCP 层问题，目标板仍可交互。
        active/uboot 不检测: 系统正常运行中。
        """
        if self._current != "booting":
            self._hang_count = 0
            return
        elapsed = time.monotonic() - self._last_data_time
        if elapsed > self._hang_timeout:
            self._hang_count += 1
            if self._hang_count >= self._hysteresis:
                self.transition("DUT-off")
        else:
            self._hang_count = 0

    def _atomic_write(self, state: str):
        """tmp → rename 原子写。reader 不会读到半截。"""
        tmp = self._state_file.with_suffix(".tmp")
        try:
            tmp.write_text(state)
            tmp.rename(self._state_file)
            logger.debug(f"StateManager: wrote target-state={state} to {self._state_file}")
        except Exception as e:
            logger.error(f"StateManager: FAILED to write target-state={state}: {e}")

    def _delete_state_file(self):
        """删除状态文件。MCP Server 停止时调用。"""
        try:
            if self._state_file.exists():
                self._state_file.unlink()
                logger.debug(f"StateManager: deleted {self._state_file}")
        except Exception as e:
            logger.error(f"StateManager: FAILED to delete {self._state_file}: {e}")

    def _delete_pid_file(self):
        """删除 PID 文件。MCP Server 停止时调用。"""
        try:
            if self._pid_file.exists():
                self._pid_file.unlink()
                logger.debug(f"StateManager: deleted {self._pid_file}")
        except Exception as e:
            logger.error(f"StateManager: FAILED to delete {self._pid_file}: {e}")
