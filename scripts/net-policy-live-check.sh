#!/bin/bash
# Step 69 live: allow_lan=false internet vs LAN, MAC anti-spoof, iperf vs Step 68.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
assets="${root}/test-assets/virtio-user-nat"
logdir="${root}/target/net-policy-live"
mkdir -p "${logdir}"
export PATH="/tmp/iperf3-prefix/bin:${PATH}"
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-${root}/entitlements/ternvale-hv-only.entitlements}"
export TERNVALE_LOG="${TERNVALE_LOG:-ternvale::net::user=debug,info}"

log() { printf 'net-policy-live: %s\n' "$*" >&2; }
die() { log "FAIL: $*"; exit 1; }

if [[ ! -f "${assets}/Image" || ! -f "${assets}/initramfs.cpio" || ! -f "${assets}/id_ed25519" ]]; then
    log "building user-nat initramfs"
    "${script_dir}/make-user-nat-initramfs.sh" "${assets}"
fi
command -v iperf3 >/dev/null 2>&1 || die "host iperf3 not on PATH"
command -v ssh >/dev/null 2>&1 || die "host ssh not on PATH"

router=$(route -n get default 2>/dev/null | awk '/gateway:/{print $2}')
if [[ -z "${router}" ]]; then
    router="192.168.1.1"
fi
log "host default gateway ${router} (guest must not reach it)"

vm_pid=""
iperf_pid=""
burst_pid=""
cleanup() {
    if [[ -n "${vm_pid}" ]] && kill -0 "${vm_pid}" 2>/dev/null; then
        kill "${vm_pid}" 2>/dev/null || true
        wait "${vm_pid}" 2>/dev/null || true
    fi
    if [[ -n "${iperf_pid}" ]] && kill -0 "${iperf_pid}" 2>/dev/null; then
        kill "${iperf_pid}" 2>/dev/null || true
    fi
    if [[ -n "${burst_pid}" ]] && kill -0 "${burst_pid}" 2>/dev/null; then
        kill "${burst_pid}" 2>/dev/null || true
    fi
}
trap cleanup EXIT

log "host iperf3 on 127.0.0.1:5201"
iperf3 -s -B 127.0.0.1 -p 5201 >"${logdir}/iperf3-host.log" 2>&1 &
iperf_pid=$!

log "burst acceptor on 127.0.0.1:19999 (initramfs still opens 500 nc)"
python3 - "${logdir}/burst.log" <<'PY' &
import socket, sys, time
log = open(sys.argv[1], "w", buffering=1)
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", 19999))
s.listen(1024)
s.settimeout(0.5)
held = []
log.write("listening\n")
deadline = time.time() + 45
while len(held) < 500 and time.time() < deadline:
    try:
        c, _ = s.accept()
        held.append(c)
    except socket.timeout:
        continue
    except OSError:
        break
log.write(f"peak {len(held)}\n")
time.sleep(1)
for c in held:
    c.close()
log.write("closed\n")
PY
burst_pid=$!

serial="${logdir}/guest-serial.log"
: >"${serial}"
cat >"${logdir}/vm.toml" <<EOF
name = "net-policy-live"
cpus = 1
ram_mib = 512
kernel = "${assets}/Image"
initrd = "${assets}/initramfs.cpio"
cmdline = "console=ttyAMA0"
serial_log = "${serial}"

[[nics]]
backend = "user"
allow_lan = false
anti_spoof = true
deny = [
  { cidr = "224.0.0.0/4" },
  { cidr = "0.0.0.0/0", port = 25, proto = "tcp" },
]
forwards = [{ host_port = 2223, guest_port = 22, proto = "tcp" }]
EOF

log "booting guest allow_lan=false (entitlements=${TERNVALE_ENTITLEMENTS})"
cargo run -q -p ternvale-cli --manifest-path "${root}/Cargo.toml" -- \
    run "${logdir}/vm.toml" >"${logdir}/vmm.log" 2>&1 &
vm_pid=$!

wait_serial() {
    local needle=$1
    local secs=$2
    local i=0
    while [[ "${i}" -lt "${secs}" ]]; do
        if grep -a -q -- "${needle}" "${serial}" 2>/dev/null; then
            log "matched ${needle}"
            return 0
        fi
        if ! kill -0 "${vm_pid}" 2>/dev/null; then
            log "VMM log:"; tail -80 "${logdir}/vmm.log" >&2 || true
            log "serial:"; tail -50 "${serial}" >&2 || true
            die "VMM exited before matching ${needle}"
        fi
        sleep 1
        i=$((i + 1))
    done
    log "serial:"; tail -80 "${serial}" >&2 || true
    die "timeout waiting for ${needle}"
}

guest_ssh() {
    ssh -T -i "${assets}/id_ed25519" -p 2223 \
        -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
        -o IdentitiesOnly=yes -o PreferredAuthentications=publickey \
        -o ConnectTimeout=8 -o BatchMode=yes \
        root@127.0.0.1 "$@"
}

wait_serial "ternvale user-nat dhcp addr=" 90
grep -q "10.0.2.15" "${serial}" || die "DHCP lease was not 10.0.2.15"
wait_serial "ternvale user-nat ping ok" 40
wait_serial "ternvale user-nat curl ok" 40
wait_serial "ternvale user-nat sshd ready" 20

log "ssh: ping internet 1.1.1.1"
guest_ssh 'ping -c 2 -W 3 1.1.1.1' >"${logdir}/ping-wan.out" 2>"${logdir}/ping-wan.err" \
    || { cat "${logdir}/ping-wan.err" >&2; die "guest cannot reach 1.1.1.1"; }
grep -q "bytes from" "${logdir}/ping-wan.out" || die "wan ping had no replies"

log "ssh: ping LAN ${router} (must fail)"
if guest_ssh "ping -c 2 -W 2 ${router}" >"${logdir}/ping-lan.out" 2>"${logdir}/ping-lan.err"; then
    cat "${logdir}/ping-lan.out" >&2
    die "guest reached LAN ${router} with allow_lan=false"
fi
if ! grep -a "policy drop" "${logdir}/vmm.log" | grep -q allow_lan; then
    tail -40 "${logdir}/vmm.log" >&2
    die "no allow_lan policy drop in VMM log"
fi
log "LAN blocked (allow_lan drop logged)"

wait_serial "iperf Done" 90
wait_serial "ternvale user-nat ready" 20

log "ssh: spoof MAC then ping 1.1.1.1"
# After the MAC change, guest TX (including this SSH session) is dropped.
ssh -T -i "${assets}/id_ed25519" -p 2223 \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o IdentitiesOnly=yes -o PreferredAuthentications=publickey \
    -o ConnectTimeout=8 -o BatchMode=yes \
    -o ServerAliveInterval=1 -o ServerAliveCountMax=3 \
    root@127.0.0.1 \
    'ip link set eth0 down; ip link set eth0 address 52:54:00:00:00:99; ip link set eth0 up; ping -c 3 -W 1 1.1.1.1; echo spoof-ping-done' \
    >"${logdir}/spoof.out" 2>"${logdir}/spoof.err" &
spoof_pid=$!
spoof_wait=0
while [[ "${spoof_wait}" -lt 20 ]] && kill -0 "${spoof_pid}" 2>/dev/null; do
    sleep 1
    spoof_wait=$((spoof_wait + 1))
done
if kill -0 "${spoof_pid}" 2>/dev/null; then
    kill "${spoof_pid}" 2>/dev/null || true
    wait "${spoof_pid}" 2>/dev/null || true
fi
sleep 1
if grep -a "policy drop" "${logdir}/vmm.log" | grep -q spoof_mac; then
    log "anti-spoof drop logged (spoof_mac)"
else
    tail -80 "${logdir}/vmm.log" >&2
    die "no spoof_mac policy drop after guest MAC change"
fi

if ! kill -0 "${vm_pid}" 2>/dev/null; then
    die "VMM exited before ready"
fi

log "PASS"
grep -E "ternvale user-nat|iperf3|receiver|sender|policy drop|rule=" \
    "${serial}" "${logdir}/iperf3-host.log" "${logdir}/vmm.log" "${logdir}/ping-wan.out" || true
