#!/usr/bin/env python3
"""Boot stage detector — 逐行扫描串口输出，检测启动阶段并触发回调。

仿 lava_dispatcher Pipeline: ConnectDevice → ResetDevice → BootloaderInterrupt →
BootloaderCommands → AutoLogin → ExpectShellSession
简化为: 逐行扫描 + 回调触发。"""

import re
import time
from dataclasses import dataclass, field
from typing import Callable


@dataclass
class BootStage:
    """仿 lava Action 粒度"""
    name: str
    pattern: re.Pattern
    on_enter_state: str
    action: str | None = None  # "rotate_log"|"send_ctrl_c"|"send_login"|"send_password"


BOOT_STAGES = [
    # Bootloader stages
    BootStage("spl",    re.compile(rb"U-Boot\s+SPL"),           "booting", "rotate_log"),
    BootStage("tpl",    re.compile(rb"TL[123]\s"),              "booting", None),
    BootStage("bl31",   re.compile(rb"BL31:"),                  "booting", None),
    BootStage("optee",  re.compile(rb"OP-TEE"),                 "booting", None),
    BootStage("ddr",    re.compile(rb"DDR\s+Version"),          "booting", None),
    BootStage("uboot",  re.compile(rb"U-Boot\s+20\d{2}"),       "uboot", None),
    BootStage("autoboot",
        re.compile(rb"Hit\s+(?:any\s+)?key\s+to\s+stop\s+autoboot"),
        "uboot", "send_ctrl_c"),
    # Kernel
    BootStage("kernel", re.compile(rb"Linux\s+version"),        "booting", None),
    BootStage("start",  re.compile(rb"Starting\s+kernel"),      "booting", None),
    # Linux shell (Debian/Ubuntu style)
    BootStage("login",  re.compile(rb"(?:.*\s)?login:\s*$"),    "booting", "send_login"),
    BootStage("password", re.compile(rb"Password:\s*$"),        "booting", "send_password"),
    # Linux shell prompt (root@host:~# or user@host:~$) — 行末的 # 或 $
    BootStage("shell",  re.compile(rb"[#\$]\s*$"),              "booted", None),
    # Android shell prompt (console:/ $ or / #)
    BootStage("android_shell",
        re.compile(rb"console:/\s+#\s*$"),
        "booted", None),
    # Android boot completion signals
    BootStage("android_init",
        re.compile(rb"init:\s*(?:started service|starting service)"),
        "booting", None),
    BootStage("android_adbd",
        re.compile(rb"adbd?\s+(?:.*?\s)?(?:starting|started|ready)"),
        "booted", None),
    BootStage("android_bootanim",
        re.compile(rb"bootanim\s+(?:service\s+)?(?:started|stopped|done)"),
        "booted", None),
    BootStage("android_surfaceflinger",
        re.compile(rb"surfaceflinger\s+.*?(?:started|ready)"),
        "booted", None),
    BootStage("android_boot_completed",
        re.compile(rb"(?:sys\.)?boot_completed\s*[=:]\s*(?:1|true|done)"),
        "booted", None),
]

CRASH_PATTERNS = [
    (re.compile(rb"Kernel\s+panic\s*[-:]"),             "panic"),
    (re.compile(rb"BUG:\s"),                             "BUG"),
    (re.compile(rb"Oops:\s"),                            "Oops"),
    (re.compile(rb"Unable\s+to\s+handle\s+kernel"),      "kernel-fault"),
    (re.compile(rb"BUG:\s+unable\s+to\s+handle"),        "BUG"),
    (re.compile(rb"Segmentation\s+fault"),               "segfault"),
    (re.compile(rb"---\[\s*end\s+trace\s+[0-9a-f]+\s*\]---"), "end-trace"),
]


class BootStageDetector:
    """逐行扫描 + 回调触发。仿 lava Pipeline + labgrid BootStage。"""

    def __init__(self):
        self._line_buf = b""
        self._boot_detected = False
        self._login_sent = False
        self._password_sent = False
        self._last_crash_time = 0.0
        # 回调
        self.on_boot_start: Callable[[], None] = lambda: None
        self.on_autoboot: Callable[[], None] = lambda: None
        self.on_login_prompt: Callable[[], None] = lambda: None
        self.on_password_prompt: Callable[[], None] = lambda: None
        self.on_crash: Callable[[str, bytes], None] = lambda t, l: None
        self.on_stage: Callable[[str], None] = lambda s: None
        self.on_activity: Callable[[], None] = lambda: None
        # 临时 watchers (serial_wait_pattern)
        self._watchers: list[tuple[re.Pattern, Callable[[bytes], None]]] = []

    def add_watcher(self, pattern: re.Pattern, callback: Callable[[bytes], None]):
        self._watchers.append((pattern, callback))

    def remove_watcher(self, pattern: re.Pattern):
        self._watchers = [(p, cb) for p, cb in self._watchers if p != pattern]

    def feed(self, data: bytes):
        self.on_activity()
        self._line_buf += data
        if len(self._line_buf) > 65536:
            self._line_buf = self._line_buf[-32768:]

        while b"\n" in self._line_buf or b"\r" in self._line_buf:
            line, self._line_buf = self._split_line(self._line_buf)
            if not line:
                continue
            self._check_crash(line)
            self._check_stages(line)
            self._check_watchers(line)

    def reset_cycle(self):
        """完全重置 — 新上电周期开始时调用。"""
        self._boot_detected = False
        self._login_sent = False
        self._password_sent = False

    def reset_login_state(self):
        """仅重置登录状态 — boot_start 回调使用, 不影响 _boot_detected。"""
        self._login_sent = False
        self._password_sent = False

    # ── internal ──────────────────────────────────────────

    @staticmethod
    def _split_line(buf: bytes) -> tuple[bytes, bytes]:
        idx_n = buf.find(b"\n")
        idx_r = buf.find(b"\r")
        if idx_n >= 0 and (idx_r < 0 or idx_n < idx_r):
            sep, idx = b"\n", idx_n
        elif idx_r >= 0:
            sep, idx = b"\r", idx_r
        else:
            return b"", buf
        line = buf[:idx].strip()
        return line, buf[idx + len(sep):]

    def _check_stages(self, line: bytes):
        for stage in BOOT_STAGES:
            if stage.pattern.search(line):
                self._handle_stage(stage, line)

    def _handle_stage(self, stage: BootStage, line: bytes):
        if stage.name == "spl" and self._boot_detected:
            return
        if stage.name == "spl":
            self._boot_detected = True
            self._login_sent = False
            self._password_sent = False

        match stage.action:
            case "rotate_log":
                self.on_boot_start()
            case "send_ctrl_c":
                self.on_autoboot()
            case "send_login":
                if not self._login_sent:
                    self.on_login_prompt()
                    self._login_sent = True
            case "send_password":
                if not self._password_sent:
                    self.on_password_prompt()
                    self._password_sent = True

        self.on_stage(stage.name)

    def _check_crash(self, line: bytes):
        for pat, ctype in CRASH_PATTERNS:
            if pat.search(line):
                now = time.time()
                if now - self._last_crash_time > 2.0:
                    self._last_crash_time = now
                    self.on_crash(ctype, line)

    def _check_watchers(self, line: bytes):
        for pat, cb in self._watchers:
            if pat.search(line):
                cb(line)
