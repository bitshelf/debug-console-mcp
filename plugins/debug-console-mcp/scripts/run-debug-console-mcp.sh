#!/usr/bin/env bash
set -euo pipefail

search_dir=${CODEX_WORKSPACE_ROOT:-${PWD}}
while [[ ${search_dir} != "/" ]]; do
    if [[ -f "${search_dir}/.target.jsonc" ]]; then
        target_config="${search_dir}/.target.jsonc"
        break
    fi
    search_dir=$(dirname "${search_dir}")
done

if [[ -z ${target_config:-} ]]; then
    echo "debug-console-mcp is inactive: no .target.jsonc found from ${PWD} upward" >&2
    exit 0
fi

export TARGET_CONF=${target_config}

if [[ -n ${DEBUG_CONSOLE_MCP_BIN:-} ]]; then
    if [[ ! -x ${DEBUG_CONSOLE_MCP_BIN} ]]; then
        echo "DEBUG_CONSOLE_MCP_BIN is not executable: ${DEBUG_CONSOLE_MCP_BIN}" >&2
        exit 126
    fi
    exec "${DEBUG_CONSOLE_MCP_BIN}"
fi

if executable=$(command -v debug-console-mcp 2>/dev/null); then
    exec "${executable}"
fi

for executable in \
    "${CARGO_HOME:-${HOME}/.cargo}/bin/debug-console-mcp" \
    "${HOME}/.local/bin/debug-console-mcp"
do
    if [[ -x ${executable} ]]; then
        exec "${executable}"
    fi
done

echo "debug-console-mcp is required for projects with .target.jsonc." >&2
echo "Install it with: cargo install --git https://github.com/bitshelf/debug-console-mcp --locked" >&2
exit 127
