#!/bin/bash
# Loom model of the Step 25 lock order (docs/ARCHITECTURE.md "Shared state and
# lock order"): MMIO slot → serial/device → GuestMemory → VirtioIrq →
# pci-intx-pin → pci-intx-line.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

export RUSTFLAGS="${RUSTFLAGS:-} --cfg loom"
export LOOM_MAX_PREEMPTIONS="${LOOM_MAX_PREEMPTIONS:-3}"

echo "loom: lock_order_loom (Step 25)"
cargo test -p ternvale-log --test lock_order_loom -- --nocapture
echo "loom: ok"
