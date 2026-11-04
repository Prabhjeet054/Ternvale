#!/bin/bash
# Step 67 live check: two host-only guests ping; internet blocked; bridged
# without com.apple.vm.networking prints docs/ENTITLEMENT_REQUEST.md.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
assets="${root}/test-assets/virtio-vmnet"
sock="${TERNVALE_VMNET_SOCK:-/tmp/ternvale-vmnet-modes.sock}"
logdir="${root}/target/vmnet-modes"
mkdir -p "${logdir}"
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-${root}/entitlements/ternvale-hv-only.entitlements}"
identity="${TERNVALE_CODESIGN_IDENTITY:-Apple Development: prabhjeetsingh054@gmail.com (687DRQ53JY)}"
helper_ent="${root}/entitlements/ternvale-vmnetd-unpriv.entitlements"

log() { printf 'vmnet-modes: %s\n' "$*" >&2; }
die() { log "FAIL: $*"; exit 1; }

helper_pid=""
vm_a=""
vm_b=""
cleanup() {
    for pid in "${vm_a:-}" "${vm_b:-}" "${helper_pid:-}"; do
        if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
            kill "${pid}" 2>/dev/null || true
            wait "${pid}" 2>/dev/null || true
        fi
    done
}
trap cleanup EXIT

sign_helper() {
    cp -f "$1" "$2"
    codesign --sign "${identity}" --force --entitlements "${helper_ent}" "$2" >/dev/null
}

wait_log() {
    local file=$1 needle=$2 secs=$3
    local i=0
    while [[ "${i}" -lt "${secs}" ]]; do
        if grep -a -q -- "${needle}" "${file}" 2>/dev/null; then
            log "matched ${needle} in $(basename "${file}")"
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    log "serial/log $(basename "${file}"):"
    tail -80 "${file}" >&2 || true
    die "timeout waiting for ${needle} in ${file}"
}

start_helper() {
    rm -f "${sock}"
    log "starting signed helper on ${sock}"
    env TERNVALE_VMNET_SOCK="${sock}" /tmp/ternvale-vmnetd >"${logdir}/helper.log" 2>&1 &
    helper_pid=$!
    local ok=0
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
    [[ "${ok}" -eq 1 ]] || { cat "${logdir}/helper.log" >&2; die "helper did not listen"; }
}

write_vm_toml() {
    local name=$1 serial=$2 extra_cmdline=$3 nic=$4
    cat >"${logdir}/${name}.toml" <<EOF
name = "${name}"
cpus = 1
ram_mib = 256
kernel = "${assets}/Image"
initrd = "${assets}/initramfs.cpio"
cmdline = "console=ttyAMA0 ${extra_cmdline}"
serial_log = "${serial}"

${nic}
EOF
}

boot_vm() {
    local name=$1
    local serial="${logdir}/${name}-serial.log"
    : >"${serial}"
    env TERNVALE_VMNET_SOCK="${sock}" TERNVALE_PCAP="${logdir}/${name}.pcap" \
        "${vmm_bin}" run "${logdir}/${name}.toml" \
        >"${logdir}/${name}-vmm.log" 2>&1 &
    echo $!
}

if [[ "${TERNVALE_SKIP_INITRAMFS:-}" == 1 && -f "${assets}/Image" && -f "${assets}/initramfs.cpio" ]]; then
    log "reusing ${assets} (TERNVALE_SKIP_INITRAMFS=1)"
else
    log "rebuilding host-only-capable initramfs"
    ./scripts/make-vmnet-initramfs.sh "${assets}"
fi
[[ -f "${assets}/Image" && -f "${assets}/initramfs.cpio" ]] || die "initramfs build did not write Image/initramfs.cpio"

log "building ternvale-cli --features vmnet and ternvale-vmnetd"
cargo build -q -p ternvale-cli --features vmnet --manifest-path "${root}/Cargo.toml"
cargo build -q -p ternvale-vmnetd --manifest-path "${root}/Cargo.toml" \
    --config 'target.aarch64-apple-darwin.runner="/usr/bin/env"'

helper_src="${CARGO_TARGET_DIR:-${root}/target}/debug/ternvale-vmnetd"
[[ -x "${helper_src}" ]] || helper_src="${root}/target/debug/ternvale-vmnetd"
[[ -x "${helper_src}" ]] || die "ternvale-vmnetd not built"
pkill -f '/tmp/ternvale-vmnetd' 2>/dev/null || true
sleep 0.2
sign_helper "${helper_src}" /tmp/ternvale-vmnetd
vmm_src="${CARGO_TARGET_DIR:-${root}/target}/debug/ternvale"
[[ -x "${vmm_src}" ]] || vmm_src="${root}/target/debug/ternvale"
[[ -x "${vmm_src}" ]] || die "ternvale CLI not built"
vmm_bin=/tmp/ternvale-modes
cp -f "${vmm_src}" "${vmm_bin}"
codesign --sign "${identity}" --force --entitlements "${TERNVALE_ENTITLEMENTS}" "${vmm_bin}" >/dev/null
start_helper

host_nic='[[nics]]
backend = "vmnet"
mode = "host-only"
network = "lab"'

write_vm_toml hostonly-a "${logdir}/hostonly-a-serial.log" \
    "ternvale.ip=192.168.100.10/24 ternvale.peer=192.168.100.11" "${host_nic}"
write_vm_toml hostonly-b "${logdir}/hostonly-b-serial.log" \
    "ternvale.ip=192.168.100.11/24 ternvale.peer=192.168.100.10" "${host_nic}"

log "booting host-only guest a=192.168.100.10"
vm_a=$(boot_vm hostonly-a)
wait_log "${logdir}/hostonly-a-serial.log" "ternvale vmnet static addr=" 60
log "booting host-only guest b=192.168.100.11"
vm_b=$(boot_vm hostonly-b)
wait_log "${logdir}/hostonly-b-serial.log" "ternvale vmnet static addr=" 60

wait_log "${logdir}/hostonly-a-serial.log" "ternvale vmnet ping ok" 100
wait_log "${logdir}/hostonly-b-serial.log" "ternvale vmnet ping ok" 40
wait_log "${logdir}/hostonly-a-serial.log" "ternvale vmnet internet blocked" 20
wait_log "${logdir}/hostonly-b-serial.log" "ternvale vmnet internet blocked" 20
grep -q "FAIL internet reachable" "${logdir}/hostonly-a-serial.log" \
    && die "guest a reached the internet"
grep -q "FAIL internet reachable" "${logdir}/hostonly-b-serial.log" \
    && die "guest b reached the internet"
log "host-only: guests ping each other; HTTPS to example.com blocked"

kill "${vm_a}" "${vm_b}" 2>/dev/null || true
wait "${vm_a}" 2>/dev/null || true
wait "${vm_b}" 2>/dev/null || true
vm_a=""
vm_b=""
sleep 1

bridges=$(/tmp/ternvale-vmnetd --list-bridges || true)
log "bridgeable interfaces: ${bridges:-<none>}"
bridge_if=$(printf '%s\n' "${bridges}" | awk 'NF{print; exit}')
bridge_if="${bridge_if:-en0}"

bridged_nic="[[nics]]
backend = \"vmnet\"
mode = \"bridged\"
bridge_interface = \"${bridge_if}\""

write_vm_toml bridged-unpriv "${logdir}/bridged-unpriv-serial.log" "" "${bridged_nic}"
log "booting bridged guest (virtualization-signed helper, no com.apple.vm.networking)"
vm_a=$(boot_vm bridged-unpriv)
denied=0
leased=0
for _ in $(seq 1 50); do
    if grep -a -q "ENTITLEMENT_REQUEST.md" "${logdir}/bridged-unpriv-vmm.log" 2>/dev/null \
        || grep -a -qi "com.apple.vm.networking" "${logdir}/bridged-unpriv-vmm.log" 2>/dev/null; then
        denied=1
        break
    fi
    if grep -a -q "ternvale vmnet dhcp addr=" "${logdir}/bridged-unpriv-serial.log" 2>/dev/null; then
        leased=1
        break
    fi
    sleep 0.5
done
if [[ "${denied}" -eq 1 ]]; then
    kill "${vm_a}" 2>/dev/null || true
    wait "${vm_a}" 2>/dev/null || true
    vm_a=""
    log "CLI bridged denial points at docs/ENTITLEMENT_REQUEST.md (or com.apple.vm.networking)"
    if ! grep -a -qiE "com.apple.vm.networking|not_authorized|invalid_access|bridged mode needs" \
        "${logdir}/bridged-unpriv-vmm.log"; then
        die "app matcher would miss this CLI error"
    fi
    log "app matcher would rewrite this as the bridged entitlement explanation"
elif [[ "${leased}" -eq 1 ]]; then
    addr=$(grep -a "ternvale vmnet dhcp addr=" "${logdir}/bridged-unpriv-serial.log" | tail -1)
    log "bridged guest got ${addr} (LAN DHCP; Darwin 27 allowed VMNET_BRIDGED_MODE with virtualization)"
    for _ in $(seq 1 25); do
        grep -aq "ternvale vmnet curl ok" "${logdir}/bridged-unpriv-serial.log" && break
        sleep 1
    done
    kill "${vm_a}" 2>/dev/null || true
    wait "${vm_a}" 2>/dev/null || true
    vm_a=""
    if grep -q "192.168.64." "${logdir}/bridged-unpriv-serial.log"; then
        die "bridged lease looks like shared NAT (192.168.64.x), not the LAN"
    fi
    log "bridged LAN DHCP verified (reachability from a second physical device not probed)"
else
    log "VMM log:"; tail -40 "${logdir}/bridged-unpriv-vmm.log" >&2 || true
    log "serial:"; tail -20 "${logdir}/bridged-unpriv-serial.log" >&2 || true
    kill "${vm_a}" 2>/dev/null || true
    wait "${vm_a}" 2>/dev/null || true
    vm_a=""
    die "bridged neither denied with docs/ENTITLEMENT_REQUEST.md nor got a DHCP lease"
fi

log "PASS"
