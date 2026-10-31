#!/bin/bash
# Step 61 test: live `ternvale stats --watch` during virtio-blk `dd`, then five
# initrd boots with metrics on vs compile-time metrics off. Fails if mean
# boot_ms with metrics is more than 1% slower.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

runs="${TERNVALE_METRICS_BOOTS:-5}"
log_root="${root}/target/metrics-check"

log() {
    printf 'metrics-check: %s\n' "$*"
}

extract_boot_ms() {
    local dir=$1
    # tracing field from ternvale-devices/tests/boot/initrd.rs
    grep -RhoE 'boot_ms=[0-9]+' "$dir" 2>/dev/null | sed 's/boot_ms=//' | tail -1
}

mean() {
    awk 'BEGIN { n=0; s=0 } { n++; s+=$1 } END { if (n==0) { print "0" } else { printf "%.3f", s/n } }'
}

boot_series() {
    local label=$1
    shift
    local -a ms=()
    local i dir value
    for i in $(seq 1 "$runs"); do
        dir="${log_root}/${label}-${i}"
        mkdir -p "$dir"
        log "boot ${label} ${i}/${runs} logs ${dir}"
        TERNVALE_BOOT_SCENARIO=initrd TERNVALE_BOOT_LOG_DIR="$dir" \
            cargo test -p ternvale-devices "$@" --test boot -- --nocapture \
            >"${dir}/cargo.out" 2>&1
        value=$(extract_boot_ms "$dir")
        if [[ -z "$value" ]]; then
            log "error: no boot_ms in ${dir}"
            exit 1
        fi
        log "boot ${label} ${i} boot_ms=${value}"
        ms+=("$value")
    done
    printf '%s\n' "${ms[@]}"
}

mkdir -p "$log_root"

log "live stats --watch during virtio-blk dd"
# The integration binary only spawns the already-signed `ternvale` helper;
# skip sign-and-run on the test harness (it SIGKILLs in this environment).
cargo test -p ternvale-cli --test stats \
    --config 'target.aarch64-apple-darwin.runner="/usr/bin/env"' \
    -- --ignored --nocapture --test-threads=1

log "warmup metrics-on initrd"
TERNVALE_BOOT_SCENARIO=initrd TERNVALE_BOOT_LOG_DIR="${log_root}/warmup-on" \
    cargo test -p ternvale-devices --features boot-test --test boot -- --nocapture >/dev/null
log "warmup metrics-off initrd"
TERNVALE_BOOT_SCENARIO=initrd TERNVALE_BOOT_LOG_DIR="${log_root}/warmup-off" \
    cargo test -p ternvale-devices --no-default-features --features boot-test --test boot -- --nocapture >/dev/null

mapfile -t on_ms < <(boot_series on --features boot-test)
mapfile -t off_ms < <(boot_series off --no-default-features --features boot-test)

on_mean=$(printf '%s\n' "${on_ms[@]}" | mean)
off_mean=$(printf '%s\n' "${off_ms[@]}" | mean)
overhead=$(awk -v on="$on_mean" -v off="$off_mean" 'BEGIN {
    if (off <= 0) { print "nan"; exit }
    printf "%.3f", (on - off) * 100.0 / off
}')

log "metrics-on boot_ms: ${on_ms[*]} mean=${on_mean}"
log "metrics-off boot_ms: ${off_ms[*]} mean=${off_mean}"
log "overhead_pct=${overhead} (must be <= 1.0)"

awk -v o="$overhead" 'BEGIN { if (o+0 > 1.0) exit 1 }'
log "ok"
