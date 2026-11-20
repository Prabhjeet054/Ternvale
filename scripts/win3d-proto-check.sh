#!/usr/bin/env bash
# Step 108 host-side prototype checks (no Windows guest, no vkcube).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
python3 drivers/tvvk/test_icd.py
cc -c -std=c11 -Wall -Werror -o /tmp/tvvk_encode.o "$ROOT/drivers/tvvk/encode.c"
rm -f /tmp/tvvk_encode.o
cargo test -p ternvale-gpu3d-host --lib -- --test-threads=1 encode_nop guest_icd
echo "win3d-proto-check: ok"
