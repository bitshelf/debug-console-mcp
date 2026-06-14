#!/usr/bin/env python3
"""Tests for gen_marker() — 仿 labgrid util/marker.py."""

import pytest
from marker import gen_marker


class TestMarker:
    def test_marker_length_is_10(self):
        m = gen_marker()
        assert len(m) == 10

    def test_marker_no_rid_chars(self):
        """验证 marker 不包含 R, I, D 字符"""
        for _ in range(100):
            m = gen_marker()
            assert "R" not in m
            assert "I" not in m
            assert "D" not in m

    def test_marker_is_uppercase(self):
        for _ in range(100):
            m = gen_marker()
            assert m.isupper()

    def test_marker_uniqueness(self):
        """1000 个 marker 应该全部不同"""
        markers = {gen_marker() for _ in range(1000)}
        assert len(markers) == 1000
