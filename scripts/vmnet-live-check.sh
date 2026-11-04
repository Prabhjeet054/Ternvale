#!/bin/bash
# Step 66 live check: helper + Linux guest DHCP, DNS, HTTPS, iperf3, link flap.
#
# On macOS 26+, the helper is signed with com.apple.security.virtualization (no root).
# The VMM is signed with hypervisor-only entitlements (device-access SIGKILLs AMFI).
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
assets="${root}/test-assets/virtio-vmnet"
sock="${TERNVALE_VMNET_SOCK:-/tmp/ternvale-vmnet-live.sock}"
logdir="${root}/target/vmnet-live"
mkdir -p "${logdir}"
export PATH="/tmp/iperf3-prefix/bin:${PATH}"
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-${root}/entitlements/ternvale-hv-only.entitlements}"
identity="${TERNVALE_CODESIGN_IDENTITY:-Apple Development: prabhjeetsingh054@gmail.com (687DRQ53JY)}"
helper_ent="${root}/entitlements/ternvale-vmnetd-unpriv.entitlements"

log() { printf 'vmnet-live: %s\n' "$*" >&2; }
die() { log "FAIL: $*"; exit 1; }

for f in Image initramfs.cpio; do
    [[ -f "${assets}/${f}" ]] || die "missing ${assets}/${f} (run ./scripts/make-vmnet-initramfs.sh)"
done
command -v iperf3 >/dev/null 2>&1 || die "host iperf3 not on PATH"

helper_pid=""
iperf_pid=""
vm_pid=""
cleanup() {
    if [[ -n "${vm_pid}" ]] && kill -0 "${vm_pid}" 2>/dev/null; then
        kill "${vm_pid}" 2>/dev/null || true
        wait "${vm_pid}" 2>/dev/null || true
    fi
    if [[ -n "${iperf_pid}" ]] && kill -0 "${iperf_pid}" 2>/dev/null; then
        kill "${iperf_pid}" 2>/dev/null || true
    fi
    if [[ -n "${helper_pid}" ]] && kill -0 "${helper_pid}" 2>/dev/null; then
        kill "${helper_pid}" 2>/dev/null || true
        wait "${helper_pid}" 2>/dev/null || true
    fi
}
trap cleanup EXIT

sign_helper() {
    local src=$1 dest=$2
    cp -f "${src}" "${dest}"
    codesign --sign "${identity}" --force --entitlements "${helper_ent}" "${dest}" >/dev/null
}

log "building ternvale-cli --features vmnet and ternvale-vmnetd"
cargo build -q -p ternvale-cli --features vmnet --manifest-path "${root}/Cargo.toml"
cargo build -q -p ternvale-vmnetd --manifest-path "${root}/Cargo.toml" \
    --config 'target.aarch64-apple-darwin.runner="/usr/bin/env"'

helper_src="${CARGO_TARGET_DIR:-${root}/target}/debug/ternvale-vmnetd"
[[ -x "${helper_src}" ]] || helper_src="${root}/target/debug/ternvale-vmnetd"
if [[ ! -x "${helper_src}" ]]; then
    helper_src=$(find /var/folders -name ternvale-vmnetd -type f 2>/dev/null | head -1 || true)
fi
[[ -x "${helper_src}" ]] || die "ternvale-vmnetd not built"
helper_bin=/tmp/ternvale-vmnetd
# Drop any previous helper so we own the socket and vmnet interface.
pkill -f '/tmp/ternvale-vmnetd' 2>/dev/null || true
sleep 0.2
sign_helper "${helper_src}" "${helper_bin}"
rm -f "${sock}"
log "starting signed helper on ${sock}"
env TERNVALE_VMNET_SOCK="${sock}" "${helper_bin}" >"${logdir}/helper.log" 2>&1 &
helper_pid=$!
ok=0
for _ in $(seq 1 40); do
    if ! kill -0 "${helper_pid}" 2>/dev/null; then
        cat "${logdir}/helper.log" >&2 || true
        die "helper exited"
    fi
    if grep -a -q "vmnet helper listening" "${logdir}/helper.log"; then
        ok=1
        break
    fi
    sleep 0.25
done
if [[ "${ok}" -ne 1 ]]; then
    cat "${logdir}/helper.log" >&2 || true
    die "helper did not bring vmnet up"
fi

host_ip=$(ifconfig bridge100 2>/dev/null | awk '/inet /{print $2; exit}')
host_ip="${host_ip:-192.168.64.1}"
log "host iperf3 on ${host_ip}:5201"
iperf3 -s -B "${host_ip}" -p 5201 >"${logdir}/iperf3-host.log" 2>&1 &
iperf_pid=$!

serial="${logdir}/guest-serial.log"
: >"${serial}"
cat >"${logdir}/vm.toml" <<EOF
name = "vmnet-live"
cpus = 1
ram_mib = 512
kernel = "${assets}/Image"
initrd = "${assets}/initramfs.cpio"
cmdline = "console=ttyAMA0"
serial_log = "${serial}"

[[nics]]
backend = "vmnet"
EOF

log "booting guest (entitlements=${TERNVALE_ENTITLEMENTS})"
export TERNVALE_VMNET_SOCK="${sock}"
export TERNVALE_CODESIGN_IDENTITY="${identity}"
cargo run -q -p ternvale-cli --features vmnet --manifest-path "${root}/Cargo.toml" -- \
    run "${logdir}/vm.toml" >"${logdir}/vmm.log" 2>&1 &
vm_pid=$!

wait_serial() {
    local needle=$1
    local secs=$2
    local skip=${3:-0}
    local i=0
    while [[ "${i}" -lt "${secs}" ]]; do
        if tail -c "+$((skip + 1))" "${serial}" 2>/dev/null | grep -a -q -- "${needle}"; then
            log "matched ${needle}"
            return 0
        fi
        if ! kill -0 "${vm_pid}" 2>/dev/null; then
            log "VMM log:"; cat "${logdir}/vmm.log" >&2 || true
            log "serial:"; tail -50 "${serial}" >&2 || true
            die "VMM exited before matching ${needle}"
        fi
        sleep 1
        i=$((i + 1))
    done
    log "serial:"; tail -80 "${serial}" >&2 || true
    die "timeout waiting for ${needle}"
}

wait_serial "ternvale vmnet dhcp addr=" 90
if grep -q "ternvale vmnet FAIL dhcp" "${serial}"; then
    die "guest DHCP failed"
fi
if grep -q "ternvale vmnet dhcp addr= gw=" "${serial}"; then
    die "guest DHCP produced an empty lease"
fi
wait_serial "ternvale vmnet dns ok" 40
wait_serial "ternvale vmnet curl ok" 40
wait_serial "iperf Done" 40
wait_serial "ternvale vmnet ready" 20

mark=$(wc -c < "${serial}" | tr -d ' ')
log "killing helper pid ${helper_pid}"
kill "${helper_pid}" || true
wait "${helper_pid}" 2>/dev/null || true
helper_pid=""
wait_serial "ternvale vmnet carrier=0" 25 "${mark}"
tail -c "+$((mark + 1))" "${serial}" | grep -a -q "NO-CARRIER\|carrier=0" \
    || die "guest serial never showed NO-CARRIER / carrier=0"

mark=$(wc -c < "${serial}" | tr -d ' ')
log "restarting helper"
sign_helper "${helper_src}" "${helper_bin}"
env TERNVALE_VMNET_SOCK="${sock}" "${helper_bin}" >"${logdir}/helper-restart.log" 2>&1 &
helper_pid=$!
rok=0
for _ in $(seq 1 40); do
    if ! kill -0 "${helper_pid}" 2>/dev/null; then
        cat "${logdir}/helper-restart.log" >&2 || true
        die "restarted helper exited"
    fi
    if grep -a -q "vmnet helper listening" "${logdir}/helper-restart.log"; then
        rok=1
        break
    fi
    sleep 0.25
done
[[ "${rok}" -eq 1 ]] || { cat "${logdir}/helper-restart.log" >&2; die "restarted helper did not listen"; }
wait_serial "ternvale vmnet carrier=1" 40 "${mark}"

log "PASS"
grep -E "ternvale vmnet|iperf3|receiver|sender" "${serial}" "${logdir}/iperf3-host.log" || true
