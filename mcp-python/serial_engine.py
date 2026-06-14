#!/usr/bin/env python3
"""SerialEngine — 核心引擎, 协调 serial, log, detector, state, commands, relay。

Lifespan: startup → 打开串口 + 启动读循环 + 看门狗
          shutdown → 关闭一切 + 释放资源
"""

import asyncio
import logging
import time
from pathlib import Path

import serial as pyserial

from config import load_config
from console import SerialConsoleDriver
from boot_detector import BootStageDetector
from state_manager import StateManager
from log_manager import LogManager
from command_queue import CommandQueue
from relay_manager import RelayManager
from lock_manager import acquire_lock, release_lock

logger = logging.getLogger(__name__)


class SerialEngine:
    """核心引擎 — 持有所有子系统。"""

    def __init__(self, config: dict):
        self._cfg = config
        self._project_dir = Path(config["_PROJECT_DIR"])
        self._host = config["RK_DEV_HOST_IP"]
        self._port = int(config["RK_SERIAL_PORT"])

        # 子系统 (全部从 config 构造, 零硬编码)
        self.console: SerialConsoleDriver | None = None
        self.detector: BootStageDetector | None = None
        self.state: StateManager | None = None
        self.logs: LogManager | None = None
        self.commands: CommandQueue | None = None
        self.relay: RelayManager | None = None

        self._running = False
        self._read_task: asyncio.Task | None = None
        self._watchdog_task: asyncio.Task | None = None

        # 登录状态
        self._login_user = config.get("RK_LOGIN_USER", "root")
        self._login_pass = config.get("RK_LOGIN_PASS", "")
        self._interrupt_strategy = config.get("RK_UBOOT_INTERRUPT_STRATEGY", "lava")

    async def start(self):
        """Lifespan.startup: lock check → open serial → start loops."""

        # ── 1. Lock check ──
        lock_dir = self._cfg.get("RK_LOCK_DIR", "/tmp/embedded-debug/locks")
        conflicting = acquire_lock(self._host, self._port, lock_dir)
        if conflicting is not None:
            raise RuntimeError(
                f"Target {self._host}:{self._port} is already in use by PID {conflicting}.\n"
                f"Another Claude Code session owns this serial connection.\n"
                f"Exit the other session first, or use a different target."
            )

        # ── 2. 创建子系统 ──
        self.state = StateManager(
            project_dir=self._project_dir,
            hang_timeout=int(self._cfg["RK_HANG_TIMEOUT"]),
            hysteresis=int(self._cfg["RK_HANG_HYSTERESIS"]),
            dut_dir=self._cfg.get("RK_DUT_DIR", ".dut-serial"),
        )
        self.logs = LogManager(
            project_dir=self._project_dir,
            max_logs=int(self._cfg["RK_MAX_ARCHIVED_LOGS"]),
            max_file_size_mb=int(self._cfg.get("RK_MAX_LOG_FILE_SIZE", "100")),
            dut_dir=self._cfg.get("RK_DUT_DIR", ".dut-serial"),
        )
        self.detector = BootStageDetector()
        self.commands = CommandQueue()
        self.relay = RelayManager(
            host=self._host,
            port=int(self._cfg.get("RK_RELAY_PORT", "0")),
            reset_channel=int(self._cfg.get("RK_RESET_CHANNEL", "0")),
            maskrom_channel=int(self._cfg.get("RK_MASKROM_CHANNEL", "0")),
        )

        # ── 3. 设置回调 ──
        # P1-7: 使用 reset_cycle() API, 不绕过封装直接 setattr
        self.detector.on_boot_start = lambda: (
            self.logs.rotate(),
            self.state.transition("booting"),
            self.detector.reset_login_state(),  # P1-7
        )
        self.detector.on_autoboot = self._on_autoboot
        self.detector.on_login_prompt = self._on_login_prompt
        self.detector.on_password_prompt = self._on_password_prompt
        self.detector.on_crash = lambda t, l: (
            self.state.transition("crashed"),
            logger.warning(f"CRASH [{t}]: {l.decode(errors='replace')}"),
        )
        # 状态转换: booting → active (启动完成)
        # 注意: active 表示 "启动完成，shell 就绪，可执行命令"
        # Android 启动完成信号: adbd/bootanim/surfaceflinger/boot_completed
        _boot_complete_stages = {
            "shell", "android_shell", "android_adbd", "android_bootanim",
            "android_surfaceflinger", "android_boot_completed",
        }
        self.detector.on_stage = lambda s: (
            self.state.transition(
                "uboot" if s in ("uboot", "autoboot") else
                "active" if s in _boot_complete_stages else  # 启动完成 → active
                "booting"
            ) if self.state.current not in ("active", "crashed") else None
        )
        self.detector.on_activity = self.state.on_activity

        # ── 4. 打开串口 ──
        self.console = SerialConsoleDriver(
            host=self._host,
            port=self._port,
            protocol=self._cfg.get("RK_SERIAL_PROTOCOL", "raw"),
            baudrate=int(self._cfg.get("RK_SERIAL_BAUDRATE", "1500000")),
            logger=logger,
        )
        try:
            self.console.on_activate()
        except (pyserial.SerialException, OSError) as e:
            logger.warning(f"Cannot open serial: {e}")
            self.state.transition("disconnected")

        # ── 5. CommandQueue 写函数 ──
        self.commands.set_write_fn(self.console._write)

        # ── 6. 打开日志 ──
        self.logs.open_current()

        # ── 7. 状态 ──
        # 串口打开成功后，探测当前串口状态:
        # - 目标在 shell → active
        # - 目标在启动中 → booting (等 boot_detector 更新)
        # - 无输出 → active (串口已通，等数据)
        if self.console.is_open:
            await self._probe_initial_state()
        else:
            self.state.transition("disconnected")

        # ── 8. 后台任务 ──
        self._running = True
        self._read_task = asyncio.create_task(self._read_loop())
        self._watchdog_task = asyncio.create_task(self._watchdog_loop())

        logger.info(f"[{self._host}:{self._port}] SerialEngine started")

    async def _probe_initial_state(self):
        """启动后探测串口当前状态，设置正确的初始状态。

        读取串口 1 秒内的数据:
        - 检测到 shell 提示符 (# 或 $) → active (启动已完成)
        - 检测到启动日志 (U-Boot/Linux) → booting (启动进行中)
        - 无任何数据 → active (串口已通，目标可能空闲)
        """
        import re
        loop = asyncio.get_running_loop()
        try:
            # 读取串口 1 秒内的数据
            data = await asyncio.wait_for(
                loop.run_in_executor(
                    None,
                    lambda: self.console._read(size=1, timeout=1.0, max_size=4096)
                ),
                timeout=2.0
            )
            if data:
                # 写入日志和检测器
                self.logs.write(data)
                self.detector.feed(data)
                # 检查是否已在 shell
                if re.search(rb"[#\$]\s*$", data):
                    self.state.transition("active")
                    logger.info("Probe: shell detected → active")
                elif re.search(rb"U-Boot|Linux version|SPL", data):
                    # booting 状态会由 boot_detector 回调设置
                    logger.info("Probe: boot output detected → waiting for detector")
                else:
                    # 有数据但不是启动日志或 shell → 假设 active
                    self.state.transition("active")
                    logger.info("Probe: data received → active")
            else:
                # 无数据但串口已通 → active
                self.state.transition("active")
                logger.info("Probe: no data but connected → active")
        except (asyncio.TimeoutError, Exception):
            # 超时无数据 → active (串口已通)
            self.state.transition("active")
            logger.info("Probe: timeout but connected → active")

    async def stop(self):
        """Lifespan.shutdown: stop loops → close serial → release lock."""
        self._running = False

        if self._read_task:
            self._read_task.cancel()
        if self._watchdog_task:
            self._watchdog_task.cancel()

        if self.console:
            self.console.on_deactivate()
        if self.relay:
            self.relay.close()
        if self.logs:
            self.logs.close()

        lock_dir = self._cfg.get("RK_LOCK_DIR", "/tmp/embedded-debug/locks")
        release_lock(self._host, self._port, lock_dir)

        self.state.transition("stopped")
        logger.info(f"[{self._host}:{self._port}] SerialEngine stopped")

    # ── 后台循环 ─────────────────────────────────────────────────

    async def _read_loop(self):
        """后台读循环: serial.read() → log + detector + command_queue

        P1-6: 追踪连续 TIMEOUT 次数，超过阈值时探测连接是否存活。
        TCP 断连时 pyserial 不会更新 is_open，必须主动探测。
        """
        import pexpect
        loop = asyncio.get_running_loop()
        _timeout_streak = 0
        _max_timeout_streak = 20  # 10s @ 0.5s interval
        while self._running:
            try:
                data = await loop.run_in_executor(
                    None, lambda: self.console._read(size=1, timeout=0.5, max_size=4096)
                )
                _timeout_streak = 0  # 有数据 → 重置
                if not data:
                    self.state.transition("disconnected")
                    await asyncio.sleep(1)
                    continue
                self.logs.write(data)
                self.detector.feed(data)
                self.commands.feed_serial_data(data)
            except pexpect.TIMEOUT:
                _timeout_streak += 1
                # P1-6: 连续无数据超阈值 → 主动探测 TCP 连接
                if _timeout_streak >= _max_timeout_streak:
                    _timeout_streak = 0
                    try:
                        # P1-5: 写 1 字节 (空写是 TCP no-op 无法触发断开检测)
                        # P0-2: 必须 await — 否则异常永远不会被捕获
                        await loop.run_in_executor(
                            None,
                            lambda: (
                                self.console._write(b"\x00"),
                                self.console._serial.flush(),
                            )
                        )
                    except (pyserial.SerialException, OSError):
                        self.state.transition("disconnected")
            except (pyserial.SerialException, OSError):
                self.state.transition("disconnected")
                await asyncio.sleep(1)

    async def _watchdog_loop(self):
        """看门狗: 重连 (指数退避) + 挂死检测"""
        backoff = 1.0
        while self._running:
            await asyncio.sleep(backoff)
            # 挂死检测
            if self.state:
                self.state.check_hang()
            # 重连: 串口断开后尝试重连
            # 注意: 重连成功后不直接设置 active — 等待 boot 检测确定正确状态
            if self.console and not self.console.is_open and self._running:
                try:
                    self.console.on_activate()
                    # 重连成功，状态会在下次收到串口数据时由 boot_detector 更新
                    backoff = 1.0
                except (pyserial.SerialException, OSError):
                    backoff = min(backoff * 2, 30.0)

    # ── callbacks ───────────────────────────────────────────────

    def _on_autoboot(self):
        """U-Boot autoboot 中断 — P1-5: 用 async task 避免阻塞事件循环。"""
        logger.info("Autoboot detected → Ctrl-C")

        async def _send_ctrl_c():
            count = 150 if self._interrupt_strategy == "aggressive" else 4
            for _ in range(count):
                if self.console and self.console.is_open:
                    self.console.sendcontrol("c")
                await asyncio.sleep(0.1)
        asyncio.create_task(_send_ctrl_c())

    def _on_login_prompt(self):
        if self._login_user:
            logger.info(f"Sending username: {self._login_user}")
            self.console.sendline(self._login_user)

    def _on_password_prompt(self):
        if self._login_pass:
            logger.info("Sending password")
            self.console.sendline(self._login_pass)

    # ── MCP Tool 接口 ──────────────────────────────────────────

    async def send_command(self, command: str, timeout: float = 90.0) -> dict:
        return await self.commands.execute_async(command, timeout)

    async def wait_pattern(self, pattern: str, timeout: float = 60.0) -> dict:
        import re
        pat = re.compile(pattern.encode())
        event = asyncio.Event()
        matched_line = []

        def cb(line: bytes):
            matched_line.append(line.decode(errors="replace"))
            event.set()

        self.detector.add_watcher(pat, cb)
        try:
            await asyncio.wait_for(event.wait(), timeout=timeout)
            return {"matched": True, "matched_line": matched_line[0] if matched_line else None}
        except asyncio.TimeoutError:
            return {"matched": False, "matched_line": None}
        finally:
            self.detector.remove_watcher(pat)

    async def reset_target(self, wait_boot: bool = True) -> dict:
        if not self.relay.configured:
            return {"success": False, "error": "No relay configured"}
        ok = self.relay.reset()
        if ok:
            self.logs.rotate()
            self.detector.reset_cycle()
            self.state.transition("booting")
            if wait_boot:
                result = await self.wait_pattern("login:", timeout=120)
                return {"success": True, "new_boot_number": self.logs.boot_number,
                        "log_path": str(self.logs.current_path),
                        "boot_complete": result["matched"]}
        return {"success": ok, "new_boot_number": self.logs.boot_number,
                "log_path": str(self.logs.current_path)}

    async def enter_uboot(self) -> dict:
        if not self.relay.configured:
            return {"success": False, "error": "No relay configured"}
        self.relay.reset()
        self.logs.rotate()
        self.detector.reset_cycle()
        # Ctrl-C flood 15s — P1-5: async 不阻塞事件循环
        for _ in range(150):
            if self.console and self.console.is_open:
                self.console.sendcontrol("c")
            await asyncio.sleep(0.1)
        result = await self.wait_pattern(r"=>|U-Boot[>]", timeout=30)
        if result["matched"]:
            self.state.transition("uboot")
            return {"success": True, "state_after": "uboot"}
        return {"success": False, "state_after": self.state.current,
                "error": "Timed out waiting for U-Boot prompt"}

    def get_state_dict(self) -> dict:
        return {
            "state": self.state.external_state,
            "boot_number": self.logs.boot_number if self.logs else 0,
            "last_data_seconds": (
                time.monotonic() - self.state._last_data_time if self.state else 0
            ),
            "log_path": str(self.logs.current_path) if self.logs and self.logs.current_path else "",
            "relay_configured": self.relay.configured if self.relay else False,
            "login_configured": bool(self._login_user),
        }
