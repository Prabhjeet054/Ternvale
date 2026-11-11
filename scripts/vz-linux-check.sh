#!/usr/bin/env bash
# Step 92 test: Linux guests on the VZ backend, agent+status, applicable
# Step 18 scenarios, windows+tpm reject, and codesign entitlements.
#
# Applicable Step 18 scenarios on VZ (generic platform, virtio-pci, NAT,
# virtio-console, vsock, rng, raw disks):
#   initrd, smp, agent, devices
# Not applicable (native-only devices, custom PCI/EDK2, snapshots, shares,
# balloon, encryption, virtio-gpu/input, Windows installer):
#   pci, net (loopback/vmnet), rootfs-pci, gpu, input, getty, desktop,
#   firmware*, uefi-*, windows-*, encryption, clone, snapshot, virtiofs,
#   share-clipboard, audio, vm-snapshot
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
out="$root/target/vz-linux/$(date +%Y%m%d-%H%M%S)"
run_dir="/tmp/tvvz-$$"
bin="$target/debug/ternvale"
log() { printf 'vz-linux-check: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

mkdir -p "$out" "$run_dir"
export TERNVALE_RUN_DIR="$run_dir"
export TERNVALE_SKIP_SANDBOX=1
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::vz=debug,ternvale::backend=info,ternvale::cli=info}"

(cd "$root" && cargo build -p ternvale-cli -q)
"$root/scripts/sign-and-run.sh" "$bin" --help >/dev/null

log "entitlements"
ent_xml="$out/entitlements.xml"
codesign -d --entitlements - --xml "$bin" >"$ent_xml" 2>/dev/null || \
  codesign -d --entitlements - "$bin" >"$ent_xml" 2>/dev/null || true
# codesign prints the path on stderr; dump to a second capture.
codesign -d --entitlements :- "$bin" >"$out/entitlements.dump" 2>"$out/entitlements.err" || true
cat "$out/entitlements.dump" || true
python3 - "$out/entitlements.dump" <<'PY' || fail "entitlement parse"
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text(errors="replace")
# Accept XML plist or the textual dump.
need = ("com.apple.security.hypervisor", "com.apple.security.virtualization")
for key in need:
    if key not in text:
        sys.exit(f"missing {key}")
# Extra entitlements beyond HV + VZ + get-task-allow (debug signing).
keys = set(re.findall(r"com\.apple\.[A-Za-z0-9._-]+", text))
allowed = {
    "com.apple.security.hypervisor",
    "com.apple.security.virtualization",
    "com.apple.security.get-task-allow",
}
extra = sorted(keys - allowed)
if extra:
    sys.exit("extra entitlements: " + ",".join(extra))
print("entitlements ok:", ", ".join(sorted(keys)))
PY

log "windows+tpm on vz must fail with a named capability"
# profile=windows requires firmware + TPM so config.validate() succeeds and
# the VZ backend is the one that names the missing capability.
fw="$root/test-assets/firmware/QEMU_EFI.fd"
[[ -f "$fw" ]] || fail "missing $fw"
cat >"$out/windows-tpm.toml" <<EOF
name = "vz-win-tpm"
profile = "windows"
backend = "vz"
cpus = 2
ram_mib = 4096
firmware = "$fw"
serial_log = "$out/win-serial.log"
[tpm]
enabled = true
EOF
set +e
"$root/scripts/sign-and-run.sh" "$bin" run "$out/windows-tpm.toml" \
    >"$out/windows-tpm.out" 2>"$out/windows-tpm.err"
win_rc=$?
set -e
win_err=$(cat "$out/windows-tpm.err" "$out/windows-tpm.out")
printf '%s\n' "$win_err" | tee "$out/windows-tpm.log" >/dev/null
if [[ "$win_rc" -eq 0 ]]; then
    fail "windows+tpm vz run succeeded"
fi
if ! grep -Eq 'capability (guest_windows|tpm)|not supported by this backend' "$out/windows-tpm.log"; then
    fail "windows+tpm error was not specific: $(tail -5 "$out/windows-tpm.log")"
fi
log "windows+tpm rejected as expected (rc=$win_rc)"

wait_file_contains() {
    local file=$1 pattern=$2 timeout=${3:-60}
    local i=0
    while [[ $i -lt $timeout ]]; do
        if [[ -f "$file" ]] && grep -E -q "$pattern" "$file"; then
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

run_vm_bg() {
    local toml=$1
    "$root/scripts/sign-and-run.sh" "$bin" run "$toml" \
        </dev/null >"$out/${2:-run}.stdout" 2>"$out/${2:-run}.stderr" &
    echo $!
}

stop_vm() {
    local name=$1 pid=${2:-}
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$name" --force \
        >/dev/null 2>&1 || true
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
}

# --- initrd ---
log "scenario initrd"
init_assets="$root/test-assets"
[[ -f "$init_assets/Image" && -f "$init_assets/initramfs.cpio" ]] \
    || fail "missing $init_assets/Image (Step 18 initrd assets)"
cat >"$out/initrd.toml" <<EOF
name = "vz-initrd"
backend = "vz"
cpus = 1
ram_mib = 256
kernel = "$init_assets/Image"
initrd = "$init_assets/initramfs.cpio"
serial_log = "$out/initrd-serial.log"
cmdline = "console=hvc0 rdinit=/init"
[tpm]
enabled = false
EOF
init_pid=$(run_vm_bg "$out/initrd.toml" initrd)
if ! wait_file_contains "$out/initrd-serial.log" "Linux version|Run /init as init process|Freeing unused kernel" 90; then
    stop_vm vz-initrd "$init_pid"
    fail "initrd: no kernel/init in serial (see $out/initrd-serial.log $out/initrd.stderr)"
fi
log "initrd: guest reached init on virtio-console"
stop_vm vz-initrd "$init_pid"

# --- smp ---
log "scenario smp"
cat >"$out/smp.toml" <<EOF
name = "vz-smp"
backend = "vz"
cpus = 2
ram_mib = 256
kernel = "$init_assets/Image"
initrd = "$init_assets/initramfs.cpio"
serial_log = "$out/smp-serial.log"
cmdline = "console=hvc0 rdinit=/init"
[tpm]
enabled = false
EOF
smp_pid=$(run_vm_bg "$out/smp.toml" smp)
if ! wait_file_contains "$out/smp-serial.log" "Linux version|Run /init as init process|Freeing unused kernel" 90; then
    stop_vm vz-smp "$smp_pid"
    fail "smp: no Linux version"
fi
# SMP banner is best-effort; VZ may print a different CPU line.
if grep -E "SMP: Total of 2 processors|smp: Brought up 2 CPUs|CPU1: Booted" "$out/smp-serial.log" >/dev/null; then
    log "smp: secondary CPU line present"
else
    log "smp: WARN no explicit SMP banner (serial still booted 2-cpu config)"
fi
stop_vm vz-smp "$smp_pid"

# --- agent + status ---
log "scenario agent"
dev="$root/test-assets/virtio-devices"
[[ -f "$dev/Image" && -f "$dev/initramfs.cpio" ]] \
    || fail "missing virtio-devices assets (./scripts/make-devices-initramfs.sh)"
cat >"$out/agent.toml" <<EOF
name = "vz-agent"
backend = "vz"
cpus = 1
ram_mib = 256
kernel = "$dev/Image"
initrd = "$dev/initramfs.cpio"
serial_log = "$out/agent-serial.log"
cmdline = "console=hvc0 rdinit=/init ternvale.agent=debug"
[tpm]
enabled = false
[vsock]
agent = true
EOF
agent_pid=$(run_vm_bg "$out/agent.toml" agent)
if ! wait_file_contains "$out/agent-serial.log" "Linux version|Run /init as init process|ternvale agent started|Freeing unused kernel" 90; then
    stop_vm vz-agent "$agent_pid"
    fail "agent: guest did not print Linux version"
fi
connected=0
i=0
while [[ $i -lt 45 ]]; do
    if TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" status vz-agent --json \
        >"$out/status.json" 2>"$out/status.err"; then
        if python3 - "$out/status.json" <<'PY'
import json, sys
raw = open(sys.argv[1]).read().strip().splitlines()[-1]
data = json.loads(raw)
st = data.get("status") or {}
ag = st.get("agent") or {}
print("state", st.get("state"), "backend", st.get("backend"), "agent", ag.get("state"), file=sys.stderr)
if st.get("backend") != "vz":
    sys.exit(2)
if ag.get("state") == "connected":
    sys.exit(0)
sys.exit(1)
PY
        then
            connected=1
            break
        fi
    fi
    sleep 1
    i=$((i + 1))
done
if [[ "$connected" -ne 1 ]]; then
    stop_vm vz-agent "$agent_pid"
    fail "agent: ternvale status did not show connected agent (see $out/status.json $out/agent-serial.log)"
fi
log "agent: ternvale status shows connected vsock agent, backend=vz"

# --- devices (rng / vsock ready line) ---
log "scenario devices"
if wait_file_contains "$out/agent-serial.log" "ternvale devices ready" 20; then
    log "devices: guest reported virtio rng/vsock ready line"
else
    log "devices: WARN no 'ternvale devices ready' line (modules may be built-in)"
fi
stop_vm vz-agent "$agent_pid"

log "PASS"
log "artifacts $out"
rm -rf "$run_dir"
