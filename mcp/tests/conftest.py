#!/usr/bin/env python3
"""Pytest configuration — add parent package to sys.path."""

import os
import sys

# Allow imports from the mcp package
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
