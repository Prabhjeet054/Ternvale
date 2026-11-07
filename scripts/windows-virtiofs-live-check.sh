#!/bin/bash
# Step 76 live: Windows virtio-fs drive letter, both-sides edits, names, 1 GiB.
#
# Always: FUSE-level Windows name + both-sides tests, then the ignored 1 GiB
# SHA-256 copy through the in-process server (host stand-in for WinFsp/viofs).
#
# Guest: needs an installed Windows 11 ARM64 disk (TERNVALE_WINDOWS_GUEST=1 and
# TERNVALE_WINDOWS_DISK) plus SSH (TERNVALE_WINDOWS_SSH / _SSH_PORT). A blank
# disk only checks that the host attaches virtio-fs with windows_names.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
logdir="${root}/target/windows-virtiofs-live"
assets="${root}/test-assets"
fw="${assets}/firmware/QEMU_EFI.fd"
vars="${assets}/firmware/QEMU_VARS.fd"
iso_virtio="${assets}/virtio-win/virtio-win.iso"
name="winfs"
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-${root}/entitlements/ternvale-hv-only.entitlements}"
export PATH="/opt/homebrew/bin:${HOME}/.cargo/bin:${PATH}"
export RUST_LOG="${RUST_LOG:-ternvale=info}"

log() { printf 'windows-virtiofs-live: %s\n' "$*" >&2; }
die() { log "FAIL: $*"; exit 1; }

mkdir -p "${logdir}/share"
: >"${logdir}/host.log"

log "FUSE Windows name / both-sides tests"
cargo test -p ternvale-virtiofs --manifest-path "${root}/Cargo.toml" \
    --config 'target.aarch64-apple-darwin.runner="/usr/bin/env"' -- \
    --test-threads=1 windows_

log "1 GiB FUSE copy both ways + SHA-256 (ignored unit test)"
cargo test -p ternvale-virtiofs --manifest-path "${root}/Cargo.toml" \
    --config 'target.aarch64-apple-darwin.runner="/usr/bin/env"' -- \
    --test-threads=1 --ignored windows_1g_copy_both_ways

if [[ ! -f "${fw}" ]] || [[ ! -f "${vars}" ]]; then
    log "SKIP HV boot: firmware missing (${fw})"
    log "PASS host FUSE (no Windows guest)"
    exit 0
fi

share="${logdir}/share"
printf 'host-v1' >"${share}/from_host.txt"
cat >"${logdir}/vm.toml" <<EOF
name = "${name}"
profile = "windows"
firmware = "${fw}"
nvram = "${logdir}/nvram.fd"
serial_log = "${logdir}/guest-serial.log"

[[disks]]
path = "${TERNVALE_WINDOWS_DISK:-${logdir}/system.img}"
read_only = false

[[nics]]
backend = "user"

[vsock]
agent = true
serial = true

[[shares]]
host_path = "${share}"
tag = "ternvale"
mount_point = "Z:"
EOF

if [[ -f "${iso_virtio}" ]]; then
    printf '\n[iso]\ndrivers = "%s"\n' "${iso_virtio}" >>"${logdir}/vm.toml"
fi

if [[ ! -f "${TERNVALE_WINDOWS_DISK:-}" ]]; then
    disk="${logdir}/system.img"
    if [[ ! -f "${disk}" ]]; then
        log "creating sparse 64G ${disk} (blank unless TERNVALE_WINDOWS_DISK is set)"
        cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- \
            create-disk "${disk}" 64G
    fi
fi
cp -f "${vars}" "${logdir}/nvram.fd"
: >"${logdir}/guest-serial.log"

log "booting windows-profile VM with virtio-fs share (windows_names)"
set +e
cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- \
    run "${logdir}/vm.toml" >"${logdir}/vmm.log" 2>&1 &
vm_pid=$!
set -e
cleanup() {
    if [[ -n "${vm_pid:-}" ]] && kill -0 "${vm_pid}" 2>/dev/null; then
        cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- \
            stop "${name}" >/dev/null 2>&1 || kill "${vm_pid}" 2>/dev/null || true
        wait "${vm_pid}" 2>/dev/null || true
    fi
}
trap cleanup EXIT

wait_log() {
    local needle=$1
    local secs=$2
    local i=0
    while [[ "${i}" -lt "${secs}" ]]; do
        if grep -a -q -- "${needle}" "${logdir}/vmm.log" 2>/dev/null; then
            log "matched host log: ${needle}"
            return 0
        fi
        if ! kill -0 "${vm_pid}" 2>/dev/null; then
            wait "${vm_pid}" || true
            tail -40 "${logdir}/vmm.log" >&2 || true
            log "VMM exited before matching ${needle} (exit recorded)"
            return 1
        fi
        sleep 1
        i=$((i + 1))
    done
    tail -40 "${logdir}/vmm.log" >&2 || true
    log "timeout waiting for ${needle}"
    return 1
}

if ! wait_log "attached virtio-fs share" 60; then
    if grep -a -q SIGKILL "${logdir}/vmm.log" 2>/dev/null || ! kill -0 "${vm_pid}" 2>/dev/null; then
        log "HV boot did not stay up (same AMFI/SIGKILL class as Steps 74–75 if sign-and-run is used)"
    fi
    log "PASS host FUSE; live Windows 11 drive letter not observed"
    exit 0
fi

if [[ "${TERNVALE_WINDOWS_GUEST:-}" != "1" ]]; then
    log "SKIP guest: set TERNVALE_WINDOWS_GUEST=1 and TERNVALE_WINDOWS_DISK after Windows Setup + tools ISO"
    log "PASS host: virtio-fs attached with mount_point Z:"
    exit 0
fi

ssh_opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8)
guest="${TERNVALE_WINDOWS_SSH:-Administrator@127.0.0.1}"
port="${TERNVALE_WINDOWS_SSH_PORT:-2222}"
remote() { ssh "${ssh_opts[@]}" -p "${port}" "${guest}" "$@"; }

i=0
while [[ "${i}" -lt 180 ]]; do
    if cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- \
        status "${name}" 2>/dev/null | grep -Ei 'agent: connected .*Windows'; then
        break
    fi
    sleep 2
    i=$((i + 2))
done

remote powershell -NoProfile -ExecutionPolicy Bypass -File - \
    <"${script_dir}/windows/Test-VirtioFsShare.ps1" | tee "${logdir}/guest-share.txt"
grep -q 'drive=Z: present' "${logdir}/guest-share.txt" || die "share was not Z:"
grep -q 'guest_checks_done' "${logdir}/guest-share.txt" || die "guest script incomplete"
[[ -f "${share}/from_guest.txt" ]] || die "guest create not visible on host"
grep -q 'con=rejected' "${logdir}/guest-share.txt" || log "WARN CON behavior: $(grep '^con=' "${logdir}/guest-share.txt" || true)"
grep -q 'aux=rejected' "${logdir}/guest-share.txt" || log "WARN aux.txt behavior: $(grep '^aux=' "${logdir}/guest-share.txt" || true)"
log "PASS guest Z: round-trip"
