#!/bin/bash
# Step 70 live: virtio-win ARM64 drivers, Windows guest agent service, serial fallback.
#
# Host-side always: boot a windows-profile VM with virtio-console MULTIPORT and
# check `ternvale status` while waiting, plus host logs for the attach.
#
# Guest-side (needs an installed Windows 11 ARM64 disk at TERNVALE_WINDOWS_DISK,
# or vms/winagent/system.img): install vioserial + ternvale-agent.exe --install,
# reboot, confirm status shows Windows build, stop/start the service, then
# device=false serial-only reconnect.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
logdir="${root}/target/windows-agent-live"
assets="${root}/test-assets"
iso_win="${assets}/windows/windows.iso"
iso_virtio="${assets}/virtio-win/virtio-win.iso"
fw="${assets}/firmware/QEMU_EFI.fd"
vars="${assets}/firmware/QEMU_VARS.fd"
agent_exe="${root}/target/guest/ternvale-agent.exe"
name="winagent"
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-${root}/entitlements/ternvale-hv-only.entitlements}"
export PATH="/opt/homebrew/bin:${PATH}"

log() { printf 'windows-agent-live: %s\n' "$*" >&2; }
die() { log "FAIL: $*"; exit 1; }

mkdir -p "${logdir}"
serial="${logdir}/guest-serial.log"
: >"${serial}"

[[ -f "${iso_virtio}" ]] || die "missing ${iso_virtio} (./scripts/fetch-virtio-win.sh)"
[[ -f "${fw}" ]] || die "missing ${fw}"
[[ -f "${vars}" ]] || die "missing ${vars}"

log "virtio-win 0.1.302 ARM64 vioserial + viosock present"
bsdtar -tf "${iso_virtio}" | grep -qx 'vioserial/w11/ARM64/vioser.sys' \
    || die "vioserial w11 ARM64 missing"
bsdtar -tf "${iso_virtio}" | grep -qx 'viosock/w11/ARM64/viosock.sys' \
    || die "viosock w11 ARM64 missing"

if [[ ! -f "${agent_exe}" ]]; then
    log "building aarch64-pc-windows-msvc agent"
    export PATH="${HOME}/.cargo/bin:${PATH}"
    "${script_dir}/build-windows-agent.sh" "${agent_exe}"
fi
file "${agent_exe}" | grep -q 'PE32+ executable.*Aarch64' \
    || die "${agent_exe} is not a Windows ARM64 PE"

cp -f "${vars}" "${logdir}/nvram.fd"
disk="${TERNVALE_WINDOWS_DISK:-${logdir}/system.img}"
if [[ ! -f "${disk}" ]]; then
    log "creating sparse 64G ${disk} (blank; Windows Setup is not run here)"
    cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- \
        create-disk "${disk}" 64G
fi

cat >"${logdir}/vm.toml" <<EOF
name = "${name}"
profile = "windows"
firmware = "${fw}"
nvram = "${logdir}/nvram.fd"
serial_log = "${serial}"

[[disks]]
path = "${disk}"
read_only = false

[iso]
drivers = "${iso_virtio}"

[[nics]]
backend = "user"

[vsock]
device = false
serial = true
agent = true
EOF

log "booting windows-profile host (serial-only vsock.device=false)"
cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- \
    run "${logdir}/vm.toml" >"${logdir}/vmm.log" 2>&1 &
vm_pid=$!
cleanup() {
    if [[ -n "${vm_pid}" ]] && kill -0 "${vm_pid}" 2>/dev/null; then
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
            tail -80 "${logdir}/vmm.log" >&2 || true
            die "VMM exited before matching ${needle}"
        fi
        sleep 1
        i=$((i + 1))
    done
    tail -80 "${logdir}/vmm.log" >&2 || true
    die "timeout waiting for ${needle}"
}

wait_log "attached virtio-console (pci) agent port" 90
wait_log "virtio-console on virtio-pci" 10

status_txt="${logdir}/status.txt"
i=0
while [[ "${i}" -lt 30 ]]; do
    cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- \
        status "${name}" >"${status_txt}" 2>/dev/null || true
    if grep -q 'agent: waiting' "${status_txt}"; then
        log "status: agent waiting (no Windows guest agent yet)"
        cat "${status_txt}" >&2
        break
    fi
    sleep 1
    i=$((i + 1))
done
grep -q 'agent: waiting' "${status_txt}" || die "status did not show agent waiting"

if [[ "${TERNVALE_WINDOWS_GUEST:-}" != "1" ]]; then
    log "SKIP guest: no installed Windows 11 ARM64 image (set TERNVALE_WINDOWS_GUEST=1 and TERNVALE_WINDOWS_DISK=... after Setup)."
    log "blank disk will not run virtio-win pnputil or TernvaleGuestAgent."
    log "PASS host: vioserial/viosock ARM64 on ISO, agent.exe PE32+ Aarch64, virtio-console PCI attached, status waiting"
    exit 0
fi

# Guest checks (installed Windows + OpenSSH or an in-guest helper).
ssh_opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8)
guest="${TERNVALE_WINDOWS_SSH:-Administrator@127.0.0.1}"
port="${TERNVALE_WINDOWS_SSH_PORT:-2222}"
remote() { ssh "${ssh_opts[@]}" -p "${port}" "${guest}" "$@"; }

wait_status_windows() {
    local secs=$1
    local i=0
    while [[ "${i}" -lt "${secs}" ]]; do
        cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- \
            status "${name}" >"${status_txt}" 2>/dev/null || true
        if grep -Ei 'agent: connected .*Windows .*build [0-9]+' "${status_txt}"; then
            log "status connected:"
            cat "${status_txt}" >&2
            return 0
        fi
        sleep 2
        i=$((i + 2))
    done
    cat "${status_txt}" >&2 || true
    die "agent did not connect with a Windows build string"
}

wait_status_windows 180
: >"${logdir}/vmm.cut"
cp "${logdir}/vmm.log" "${logdir}/vmm.before-stop.log"
log "stopping TernvaleGuestAgent in the guest"
remote 'sc.exe stop TernvaleGuestAgent'
wait_log "agent disconnected" 30
grep -q 'agent: disconnected\|agent: waiting\|disconnect' "${status_txt}" || \
    cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- status "${name}" | tee "${status_txt}"
log "starting TernvaleGuestAgent in the guest"
remote 'sc.exe start TernvaleGuestAgent'
wait_status_windows 60
log "PASS guest service stop/start"

log "PASS"
