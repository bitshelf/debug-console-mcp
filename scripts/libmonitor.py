#!/usr/bin/env python3
"""Backward-compatibility shim for v3.0 embedded-debug.

DEPRECATED: The v3 daemon (SerialDaemon, AgentClient) has been replaced by
the v4 MCP server (see mcp/). This file exists only to avoid import errors
from stale references.

Do not add new imports here. Prefer:
    from config import load_config
"""

from config import load_config

__all__ = ["load_config"]
