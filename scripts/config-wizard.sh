#!/bin/bash
# Interactive .target.toml generator
set -e
OUT="${1:-.target.toml}"
echo "=== debug-console-mcp Config Wizard ==="
read -p "Dev Host IP [192.168.1.105]: " DH; DH="${DH:-192.168.1.105}"
read -p "Serial Port [2000]: " SP; SP="${SP:-2000}"
read -p "DUT Alias [my-board]: " ALIAS; ALIAS="${ALIAS:-my-board}"
read -p "Login User [root]: " LU; LU="${LU:-root}"

cat > "$OUT" << EOF
[[dev_hosts]]
alias = "rk-board-pc"
ip = "$DH"
user = "linaro"

[[dut]]
alias = "$ALIAS"
dev_host = "rk-board-pc"

[dut.serial]
port = $SP

[dut.target]
login_user = "$LU"

[dut.uboot]
interrupt_char = "ctrl_c"
interrupt_strategy = "aggressive"

[dut.monitor]
hang_timeout = 60
max_archived_logs = 10
reference_log = ".dut-serial/$ALIAS/reference-boot.log"
EOF

echo "Wrote: $OUT"
echo "Next: mkdir -p .dut-serial/$ALIAS/logs"
