#!/usr/bin/env python3
"""Marker generation — 仿 labgrid util/marker.py"""

import random
import string

# 排除 R, I, D 继承自 labgrid util/marker.py。
# 原因为避免 marker 包含 ERROR/FAIL/INFO/DEBUG 等日志关键字，
# 在 pexpect expect 匹配 prompt 时导致误判。
# 本设计使用数据流扫描而非 expect 匹配，理论上不需要排除，
# 但保守起见保留此策略。
MARKER_POOL = tuple(c for c in string.ascii_uppercase if c not in "RID")


def gen_marker() -> str:
    """生成 10 字符随机 marker — 仿 labgrid util/marker.py"""
    return "".join(random.choice(MARKER_POOL) for _ in range(10))
