#!/bin/bash
# Run scripts/boot-test.sh repeatedly to shake out races.
#
#   TERNVALE_BOOT_SCENARIO=smp TERNVALE_BOOT_CPUS=4 ./scripts/boot-stress.sh [runs]
#
# runs defaults to 20. Each run's console output goes to
# target/boot-logs/stress-<stamp>/run-NN.txt and summary.txt gets one line per
# run: exit status, wall seconds, ERROR / WARN / lock-wait line counts from the
# host log, the lock watch summary, and the artifact directory.
# Exits 1 if any run failed or logged a lock wait over the threshold.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

runs="${1:-20}"
if ! [[ "$runs" =~ ^[0-9]+$ ]] || [[ "$runs" -lt 1 ]]; then
    printf 'boot-stress: runs must be a positive integer, got %s\n' "$runs" >&2
    exit 2
fi
scenario="${TERNVALE_BOOT_SCENARIO:-smp}"
export TERNVALE_BOOT_SCENARIO="$scenario"
stress_dir="${root}/target/boot-logs/stress-$(date +%Y%m%d-%H%M%S)-${scenario}"
mkdir -p "$stress_dir"
summary="${stress_dir}/summary.txt"
printf 'boot-stress: %s runs of scenario %s (cpus=%s) in %s\n' \
    "$runs" "$scenario" "${TERNVALE_BOOT_CPUS:-default}" "$stress_dir" | tee "$summary"

strip() { sed 's/\x1b\[[0-9;]*m//g' "$@"; }

failed=0
lock_waits_total=0
for ((i = 1; i <= runs; i++)); do
    out="${stress_dir}/run-$(printf '%02d' "$i").txt"
    started=$(date +%s)
    set +e
    "${script_dir}/boot-test.sh" >"$out" 2>&1
    status=$?
    set -e
    seconds=$(($(date +%s) - started))
    artifacts=$(strip "$out" | sed -n 's/^boot-test: artifacts //p' | tail -1)
    host_log=$(ls "${artifacts}"/ternvale-*.log 2>/dev/null | head -1 || true)
    errors=0 warns=0 lock_waits=0 locks="-"
    if [[ -n "$host_log" ]]; then
        errors=$(strip "$host_log" | grep -c ' ERROR ' || true)
        warns=$(strip "$host_log" | grep -c ' WARN ' || true)
        lock_waits=$(strip "$host_log" | grep -cE 'lock wait exceeds threshold|lock acquired after a long wait' || true)
        locks=$(strip "$host_log" | grep 'lock watch summary' | tail -1 | grep -oE 'contended=[0-9]+ long_waits=[0-9]+ max_wait_us=[0-9]+' || true)
    fi
    if [[ "$status" -ne 0 ]]; then
        failed=$((failed + 1))
    fi
    lock_waits_total=$((lock_waits_total + lock_waits))
    printf 'run %02d exit=%s secs=%s errors=%s warns=%s lock_waits=%s %s dir=%s\n' \
        "$i" "$status" "$seconds" "$errors" "$warns" "$lock_waits" "${locks:--}" "$artifacts" | tee -a "$summary"
done

printf 'boot-stress: %s/%s passed, %s lock-wait warnings\n' \
    "$((runs - failed))" "$runs" "$lock_waits_total" | tee -a "$summary"
if [[ "$failed" -ne 0 || "$lock_waits_total" -ne 0 ]]; then
    exit 1
fi
