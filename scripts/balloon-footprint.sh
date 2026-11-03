#!/bin/bash
# Live virtio-balloon check: control-socket inflate vs process phys_footprint,
# then host `memory_pressure` for auto policy.
#
# A 4 GiB guest with ram_min_mib=1024 must already be running (`ternvale run`).
# Inside the guest, allocate and free ~3 GiB first (stress-ng or dd on tmpfs).
#
# Usage:
#   scripts/balloon-footprint.sh <vm-name> [inflate-mib]
set -euo pipefail

name=${1:?usage: balloon-footprint.sh <vm-name> [inflate-mib]}
inflate_mib=${2:-2048}

ternvale=${TERNVALE:-target/debug/ternvale}
if [[ ! -x $ternvale ]]; then
  echo "error: no ternvale binary at $ternvale (set TERNVALE=)" >&2
  exit 1
fi

pid=$(pgrep -f "ternvale run" | head -n 1 || true)
if [[ -z ${pid:-} ]]; then
  echo "error: no ternvale run process; boot a 4 GiB guest first" >&2
  exit 1
fi

footprint_bytes() {
  # `footprint` is optional; fall back to sample `phys_footprint` via vmmap summary.
  if command -v footprint >/dev/null 2>&1; then
    footprint -p "$pid" 2>/dev/null | awk '/^phys_footprint/ {print $2; exit}'
  else
    vmmap --summary "$pid" 2>/dev/null | awk '/Physical footprint/ {print $3; exit}'
  fi
}

echo "vmm pid $pid"
before=$("$ternvale" stats "$name" --json || true)
echo "stats before: $before"
fp_before=$(footprint_bytes)
echo "footprint before: ${fp_before:-unknown}"

echo "inflating balloon to ${inflate_mib} MiB via control socket"
"$ternvale" balloon "$name" --target-mib "$inflate_mib" --json
sleep 2
after=$("$ternvale" stats "$name" --json)
echo "stats after inflate: $after"
fp_after=$(footprint_bytes)
echo "footprint after inflate: ${fp_after:-unknown}"

echo "resuming auto policy"
"$ternvale" balloon "$name" --auto --json

if command -v memory_pressure >/dev/null 2>&1; then
  echo "simulating warn memory pressure for 8s (-S: no real allocation; may need root)"
  if ! memory_pressure -S -l warn -s 8; then
    echo "warning: simulated pressure failed (often Operation not permitted without root)"
  fi
  echo "stats under/after simulated pressure:"
  "$ternvale" stats "$name" --json
  echo "waiting for NORMAL after the tool exits"
  sleep 3
  "$ternvale" stats "$name" --json
else
  echo "warning: memory_pressure not on PATH; skip auto-policy live check"
fi
