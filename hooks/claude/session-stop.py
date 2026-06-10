#!/usr/bin/env python3
"""Stop hook — daemon persists across sessions. No automatic shutdown."""

import sys

# Daemon is a persistent service — do NOT stop it on session exit.
# It survives across multiple sessions and agents.
# Use `monitor.py stop` to explicitly shut it down.

if __name__ == "__main__":
    sys.exit(0)
