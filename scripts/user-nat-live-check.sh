#!/bin/bash
# Step 68 live check: user-mode NAT DHCP, ping, DNS, HTTPS, SSH forward, 500 TCP, iperf3.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
assets="${root}/test-assets/virtio-user-nat"
logdir="${root}/target/user-nat-live"
mkdir -p "${logdir}"
export PATH="/tmp/iperf3-prefix/bin:${PATH}"
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-${root}/entitlements/ternvale-hv-only.entitlements}"
export TERNVALE_LOG="${TERNVALE_LOG:-ternvale::net::user=debug,info}"

log() { printf 'user-nat-live: %s\n' "$*" >&2; }
die() { log "FAIL: $*"; exit 1; }

if [[ ! -f "${assets}/Image" || ! -f "${assets}/initramfs.cpio" || ! -f "${assets}/id_ed25519" ]]; then
    log "building user-nat initramfs"
    "${script_dir}/make-user-nat-initramfs.sh" "${assets}"
fi
command -v iperf3 >/dev/null 2>&1 || die "host iperf3 not on PATH"
command -v ssh >/dev/null 2>&1 || die "host ssh not on PATH"

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

log "burst acceptor on 127.0.0.1:19999"
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
        if len(held) % 50 == 0:
            log.write(f"held {len(held)}\n")
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
name = "user-nat-live"
cpus = 1
ram_mib = 512
kernel = "${assets}/Image"
initrd = "${assets}/initramfs.cpio"
cmdline = "console=ttyAMA0"
serial_log = "${serial}"

[[nics]]
backend = "user"
forwards = [{ host_port = 2222, guest_port = 22, proto = "tcp" }]
EOF

log "booting guest (entitlements=${TERNVALE_ENTITLEMENTS})"
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

wait_serial "ternvale user-nat dhcp addr=" 90
if grep -q "ternvale user-nat FAIL dhcp" "${serial}"; then
    die "guest DHCP failed"
fi
if grep -q "ternvale user-nat dhcp addr= gw=" "${serial}"; then
    die "guest DHCP produced an empty lease"
fi
grep -q "10.0.2.15" "${serial}" || die "DHCP lease was not 10.0.2.15"
wait_serial "ternvale user-nat ping ok" 40
wait_serial "ternvale user-nat dns ok" 40
wait_serial "ternvale user-nat curl ok" 40
wait_serial "ternvale user-nat sshd ready" 20

log "ssh -p 2222 localhost"
ssh -T -i "${assets}/id_ed25519" -p 2222 \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o IdentitiesOnly=yes -o PreferredAuthentications=publickey \
    -o ConnectTimeout=8 -o BatchMode=yes \
    root@127.0.0.1 'echo ternvale-ssh-ok' >"${logdir}/ssh.out" 2>"${logdir}/ssh.err" \
    || { cat "${logdir}/ssh.err" >&2; die "ssh -p 2222 failed"; }
grep -q "ternvale-ssh-ok" "${logdir}/ssh.out" || die "ssh output missing"

wait_serial "ternvale user-nat burst done" 90
if ! kill -0 "${vm_pid}" 2>/dev/null; then
    die "VMM died during 500-connection burst"
fi
empty=0
for _ in $(seq 1 20); do
    if grep -a -q "conntrack empty" "${logdir}/vmm.log"; then
        empty=1
        break
    fi
    sleep 1
done
[[ "${empty}" -eq 1 ]] || { tail -50 "${logdir}/vmm.log" >&2; die "conntrack did not return to empty"; }
wait_serial "iperf Done" 40
wait_serial "ternvale user-nat ready" 20

if ! kill -0 "${vm_pid}" 2>/dev/null; then
    die "VMM exited before ready"
fi

log "PASS"
grep -E "ternvale user-nat|iperf3|receiver|sender|conntrack" "${serial}" "${logdir}/iperf3-host.log" "${logdir}/vmm.log" || true
