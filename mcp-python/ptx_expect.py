#!/usr/bin/env python3
"""PtxExpect — pexpect.spawn 适配器，接 pyserial。仿 labgrid util/expect.py."""

import string
import pexpect


class PtxExpect(pexpect.spawn):
    """labgrid Wrapper of the pexpect module.

    将 ConsoleProtocol 的 read/write 桥接到 pexpect.spawn 接口。
    这样 expect() 的完整正则能力可以直接用于串口。
    """

    def __init__(self, driver):
        self.driver = driver
        self.linesep = b"\n"
        pexpect.spawn.__init__(self, None, maxread=1)

    def send(self, s):
        s = self._coerce_send_string(s)
        self._log(s, "send")
        return self.driver.write(s)

    def sendcontrol(self, char: str):
        char = char.lower()
        try:
            ord_ = string.ascii_lowercase.index(char) + 1
            self.send(bytes([ord_]))
        except ValueError:
            raise NotImplementedError(
                f"Sending control character {char} is not supported yet"
            )

    def read_nonblocking(self, size=1, timeout=-1):
        assert timeout is not None
        if timeout == -1:
            timeout = self.timeout
        return self.driver.read(size=size, timeout=timeout)
