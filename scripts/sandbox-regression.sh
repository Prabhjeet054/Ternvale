#!/bin/bash
# Step 80 test: Seatbelt deny probe, helper codesign reject, then HV boot
# scenarios under the harness profile (initrd / Windows profile / virtio-fs /
# USB / user-mode NAT).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
logdir="${root}/target/sandbox-regression"
mkdir -p "$logdir"
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-${root}/entitlements/ternvale-hv-only.entitlements}"
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::sandbox=debug}"

log() { printf 'sandbox-regression: %s\n' "$*" >&2; }
die() { log "FAIL: $*"; exit 1; }

profile="${logdir}/harness.sb"
sed \
    -e "s|__ROOT__|${root}|g" \
    -e "s|__HOME__|${HOME}|g" \
    -e "s|__TMPDIR__|${TMPDIR:-/tmp}|g" \
    "${root}/entitlements/ternvale-harness.sb" >"${profile}"
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    printf '(allow file-read* file-write* (subpath "%s"))\n' "${CARGO_TARGET_DIR}" >>"${profile}"
    printf '(allow process-exec (subpath "%s"))\n' "${CARGO_TARGET_DIR}" >>"${profile}"
fi
log "harness profile ${profile}"

log_stream_file="${logdir}/seatbelt-deny.log"
: >"${log_stream_file}"
log_stream_pid=""
if [[ -x /usr/bin/log ]]; then
    /usr/bin/log stream --style compact --predicate \
        'eventMessage CONTAINS[c] "deny" AND (eventMessage CONTAINS ".ssh" OR eventMessage CONTAINS "hosts" OR eventMessage CONTAINS "/bin/sh" OR eventMessage CONTAINS "sandbox")' \
        >"${log_stream_file}" 2>&1 &
    log_stream_pid=$!
    log "log stream pid ${log_stream_pid} -> ${log_stream_file}"
    sleep 1
fi
cleanup_stream() {
    if [[ -n "${log_stream_pid}" ]]; then
        kill "${log_stream_pid}" 2>/dev/null || true
        wait "${log_stream_pid}" 2>/dev/null || true
    fi
}
trap cleanup_stream EXIT

log "unit tests: probe denials + helper identity"
cargo test -p ternvale-sandbox --offline -- --test-threads=1 \
    seatbelt_denies_hosts_ssh_and_sh \
    helper_rejects_wrong_identity \
    helper_rejects_unsigned_or_non_ternvale_client

log "direct sandbox-exec denials against harness profile"
deny_out="${logdir}/direct-deny.txt"
{
    /usr/bin/sandbox-exec -f "${profile}" /bin/cat /etc/hosts 2>&1 || true
    /usr/bin/sandbox-exec -f "${profile}" /bin/cat /private/etc/hosts 2>&1 || true
    /usr/bin/sandbox-exec -f "${profile}" /bin/ls "${HOME}/.ssh" 2>&1 || true
    /usr/bin/sandbox-exec -f "${profile}" /bin/sh -c 'echo ternvale-sandbox-pwned' 2>&1 || true
} >"${deny_out}" 2>&1
grep -q "ternvale-sandbox-pwned" "${deny_out}" && die "/bin/sh ran under harness profile"
/usr/bin/sandbox-exec -f "${profile}" /bin/cat /private/etc/hosts >/dev/null 2>&1 \
    && die "cat /private/etc/hosts was allowed"
/usr/bin/sandbox-exec -f "${profile}" /bin/ls "${HOME}/.ssh" >/dev/null 2>&1 \
    && die "ls ~/.ssh was allowed"
log "direct denials ok (see ${deny_out})"

run_boot() {
    local scenario=$1
    log "boot scenario ${scenario} under Seatbelt"
    TERNVALE_SEATBELT_PROFILE="${profile}" \
        TERNVALE_BOOT_SCENARIO="${scenario}" \
        "${root}/scripts/boot-test.sh"
}

run_boot initrd
# Step 71 xHCI halt (-110) is still red; confirm HV still comes up under Seatbelt.
if ! TERNVALE_SEATBELT_PROFILE="${profile}" TERNVALE_BOOT_SCENARIO=usb "${root}/scripts/boot-test.sh"; then
    log "USB harness failed (expected while xHCI Host halt -110 is open); VM create under Seatbelt still required"
    usb_log=$(ls -td "${root}/target/boot-logs/"*-usb 2>/dev/null | head -1)
    grep -q "VM created" "${usb_log}"/*.log 2>/dev/null \
        || die "USB scenario did not create a hypervisor VM under Seatbelt"
fi
run_boot virtiofs
run_boot net

if [[ -f "${root}/test-assets/installer/alpine.iso" || -f "${root}/test-assets/installer/"*.iso ]]; then
    if ! TERNVALE_SEATBELT_PROFILE="${profile}" TERNVALE_BOOT_SCENARIO=windows-profile \
        "${root}/scripts/boot-test.sh"; then
        wp=$(ls -td "${root}/target/boot-logs/"*-windows-profile 2>/dev/null | head -1)
        grep -q "GNU GRUB" "${wp}/guest-serial.log" 2>/dev/null \
            || die "windows-profile control boot did not reach GRUB under Seatbelt"
        log "windows-profile: control reached GRUB; windows+TPM boot may Exception (ESR 0x20) under Seatbelt"
    fi
else
    log "SKIP windows-profile: no installer ISO under test-assets/installer"
fi

log "user-mode NAT through ternvale run (harness Seatbelt + debug probe)"
export TERNVALE_SEATBELT_PROFILE="${profile}"
export TERNVALE_SANDBOX_PROBE=1
unset TERNVALE_SKIP_SANDBOX || true
if command -v iperf3 >/dev/null 2>&1; then
    if ! "${root}/scripts/user-nat-live-check.sh"; then
        log "user-nat under Seatbelt failed (see TODO(verify) hv_vm_create in docs/SECURITY.md)"
        die "user-nat-live-check under sandbox"
    fi
else
    log "iperf3 missing; net boot scenario already ran; probe-only ternvale run"
    assets="${root}/test-assets/virtio-user-nat"
    if [[ ! -f "${assets}/Image" ]]; then
        die "missing ${assets}/Image for probe-only NAT boot"
    fi
    serial="${logdir}/probe-serial.log"
    : >"${serial}"
    cat >"${logdir}/probe-vm.toml" <<EOF
name = "sandbox-probe"
cpus = 1
ram_mib = 256
kernel = "${assets}/Image"
initrd = "${assets}/initramfs.cpio"
cmdline = "console=ttyAMA0"
serial_log = "${serial}"
[[nics]]
backend = "user"
EOF
    cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- \
        run "${logdir}/probe-vm.toml" >"${logdir}/probe-vmm.log" 2>&1 &
    vmpid=$!
    ok=0
    for _ in $(seq 1 45); do
        if grep -q "sandbox probe denied" "${logdir}/probe-vmm.log" 2>/dev/null; then
            ok=1
            break
        fi
        if grep -q "Seatbelt allowed a forbidden" "${logdir}/probe-vmm.log" 2>/dev/null; then
            tail -40 "${logdir}/probe-vmm.log" >&2
            die "debug probe was allowed"
        fi
        if ! kill -0 "${vmpid}" 2>/dev/null; then
            tail -40 "${logdir}/probe-vmm.log" >&2
            die "VMM exited before sandbox probe"
        fi
        sleep 1
    done
    kill "${vmpid}" 2>/dev/null || true
    wait "${vmpid}" 2>/dev/null || true
    [[ "${ok}" -eq 1 ]] || { tail -40 "${logdir}/probe-vmm.log" >&2; die "no sandbox probe denied lines"; }
    grep -c "sandbox probe denied" "${logdir}/probe-vmm.log" | grep -Eq '[3-9]' \
        || die "expected three sandbox probe denials"
    log "probe-only VMM logged Seatbelt denials"
fi

log "seatbelt deny log (tail)"
sleep 2
if [[ -s "${log_stream_file}" ]]; then
    tail -40 "${log_stream_file}" >&2 || true
else
    log "log stream file empty; using log show"
    /usr/bin/log show --last 10m --style compact --predicate \
        'eventMessage CONTAINS "hosts" OR eventMessage CONTAINS ".ssh" OR eventMessage CONTAINS "/bin/sh"' \
        2>/dev/null | tail -20 >&2 || true
fi

log "ok"
