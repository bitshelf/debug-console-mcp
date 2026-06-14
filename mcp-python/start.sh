#!/bin/bash
# Wrapper: start MCP server using project-local venv
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
exec "$SCRIPT_DIR/.venv/bin/python3" "$SCRIPT_DIR/server.py"
