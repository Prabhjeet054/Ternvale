#!/bin/bash
# Full regression: `make lint test test-hv`, then the boot harness scenarios.
#
#   ./scripts/regression.sh
#   TERNVALE_REGRESSION_SCENARIOS="initrd net" ./scripts/regression.sh
#
# Scenarios default to: initrd (direct kernel), rootfs (virtio-blk root), net
# (ping), smp (TERNVALE_BOOT_CPUS, default 4; the other scenarios keep their own
# CPU count), agent (vsock guest agent), firmware (UEFI banner, DTB),
# firmware-acpi (UEFI with ACPI over fw_cfg) and uefi-linux (test kernel through
# UEFI with the DTB). firmware-debug (debug EDK2 log check) is opt-in: it needs
# ./scripts/fetch-debug-firmware.sh. Each step's output goes to
# target/regression/<stamp>/<step>.txt. results.md is a markdown table of
# result, wall seconds, and the boot artifact directory. Every step runs even
# after a failure. Exits 1 if any step failed.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

scenarios="${TERNVALE_REGRESSION_SCENARIOS:-initrd rootfs net smp agent firmware firmware-acpi uefi-linux}"
smp_cpus="${TERNVALE_BOOT_CPUS:-4}"
unset TERNVALE_BOOT_CPUS
out_dir="${root}/target/regression/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out_dir"
results="${out_dir}/results.md"

strip() { sed 's/\x1b\[[0-9;]*m//g' "$@"; }

describe() {
    case "$1" in
        initrd) printf 'direct kernel + initramfs' ;;
        rootfs) printf 'virtio-blk ext4 root + host fsck' ;;
        net) printf 'virtio-net ping + pcap checks' ;;
        smp) printf 'SMP, %s vCPUs' "$smp_cpus" ;;
        agent) printf 'vsock guest agent' ;;
        firmware) printf 'UEFI banner, shell, NV variable, DTB only' ;;
        firmware-acpi) printf 'UEFI installs ACPI from fw_cfg' ;;
        firmware-debug) printf 'debug EDK2 log: ACPI from fw_cfg, no DT fallback' ;;
        uefi-linux) printf 'test kernel through UEFI, DTB only' ;;
        *) printf '%s' "$1" ;;
    esac
}

printf '| Step | What | Result | Seconds | Artifacts |\n|---|---|---|---|---|\n' >"$results"
failed=0
total_started=$(date +%s)

run_step() {
    local name="$1" what="$2"
    shift 2
    local out="${out_dir}/${name//[: ]/-}.txt"
    printf 'regression: %s ...\n' "$name"
    local started status seconds artifacts result
    started=$(date +%s)
    set +e
    "$@" >"$out" 2>&1
    status=$?
    set -e
    seconds=$(($(date +%s) - started))
    artifacts=$(strip "$out" | sed -n 's/^boot-test: artifacts //p' | tail -1)
    artifacts="${artifacts#"${root}/"}"
    if [[ "$status" -eq 0 ]]; then
        result=PASS
    else
        result="FAIL (exit ${status})"
        failed=$((failed + 1))
    fi
    printf 'regression: %s %s in %ss\n' "$name" "$result" "$seconds"
    printf '| `%s` | %s | %s | %s | %s |\n' \
        "$name" "$what" "$result" "$seconds" "${artifacts:--}" >>"$results"
}

run_step "make lint" "fmt --check, clippy -D warnings" make lint
run_step "make test" "workspace unit + integration tests" make test
run_step "make test-hv" "needs-hv tests, signed runner, serial" make test-hv
for scenario in $scenarios; do
    cpus=()
    if [[ "$scenario" == "smp" ]]; then
        cpus=(TERNVALE_BOOT_CPUS="$smp_cpus")
    fi
    run_step "boot:${scenario}" "$(describe "$scenario")" \
        env TERNVALE_BOOT_SCENARIO="$scenario" ${cpus[@]+"${cpus[@]}"} "${script_dir}/boot-test.sh"
done

printf '\n%s failed, total %ss, logs %s\n' \
    "$failed" "$(($(date +%s) - total_started))" "${out_dir#"${root}/"}" >>"$results"
cat "$results"
if [[ "$failed" -ne 0 ]]; then
    exit 1
fi
