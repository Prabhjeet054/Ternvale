#!/usr/bin/env bash
# Step 103 live: host ID_AA64* vs guest /proc/cpuinfo, baseline mask, snapshot
# restore refusal with injected IDs, and --force WARN.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
stamp="$(date +%Y%m%d-%H%M%S)"
out="$root/target/cpu-profile/${stamp}"
cache="$root/target/cpu-profile-cache"
run_dir="/tmp/tvcpu-$$"
snap_dir="$out/snapshots"
bin="$target/debug/ternvale"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"
log() { printf 'cpu-profile-live: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

mkdir -p "$out" "$cache" "$run_dir" "$snap_dir"
export TERNVALE_RUN_DIR="$run_dir"
export TERNVALE_SNAPSHOTS_DIR="$snap_dir"
export TERNVALE_SKIP_SANDBOX=1
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::cpuprofile=debug}"
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-$root/entitlements/ternvale-hv-only.entitlements}"

cleanup() {
  for n in cpu-host cpu-base cpu-snap; do
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$n" --force \
      >/dev/null 2>&1 || true
  done
  rm -rf "$run_dir"
}
trap cleanup EXIT

command -v curl >/dev/null || fail "curl required"
command -v python3 >/dev/null || fail "python3 required"

(cd "$root" && cargo build -p ternvale-cli -q)
"$root/scripts/sign-and-run.sh" "$bin" --help >/dev/null

apk_name() {
  python3 - "$1" "$2" <<'PY'
import sys
text = open(sys.argv[1], encoding="utf-8", errors="replace").read().split("\n\n")
want = sys.argv[2]
for block in text:
    fields = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
    if fields.get("P") == want:
        print(fields["P"] + "-" + fields["V"] + ".apk")
        raise SystemExit(0)
raise SystemExit(f"{want} not in APKINDEX")
PY
}

unpack_zboot() {
  python3 - "$1" "$2" <<'PY'
import gzip, pathlib, struct, sys
blob = pathlib.Path(sys.argv[1]).read_bytes()
if blob[:2] == b"\x1f\x8b":
    blob = gzip.decompress(blob)
if blob[56:60] == b"ARM\x64":
    image = blob
elif blob[4:8] == b"zimg":
    off, size = struct.unpack_from("<II", blob, 8)
    if blob[24:32].split(b"\x00", 1)[0] != b"gzip":
        sys.exit("unsupported zboot compression")
    image = gzip.decompress(blob[off:off + size])
else:
    sys.exit("vmlinuz is neither a raw arm64 Image nor EFI zboot")
if image[56:60] != b"ARM\x64":
    sys.exit("unpacked file is not an arm64 Image")
pathlib.Path(sys.argv[2]).write_bytes(image)
PY
}

wait_serial() {
  local file=$1 pattern=$2 timeout=${3:-180}
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

log "probe-cpu"
"$root/scripts/sign-and-run.sh" "$bin" probe-cpu --json >"$out/probe-cpu.json"
"$root/scripts/sign-and-run.sh" "$bin" probe-cpu >"$out/probe-cpu.txt"
grep -q hv_vcpu_config_get_feature_reg "$out/probe-cpu.txt" || fail "probe-cpu missing get_feature_reg"
grep -q "none (header has get only)" "$out/probe-cpu.txt" || fail "probe-cpu should report no set_feature_reg"

log "Alpine APKINDEX v${alpine_ver}"
curl -fsSL --retry 3 -A Ternvale -o "$cache/APKINDEX.tar.gz" \
  "${alpine}/main/aarch64/APKINDEX.tar.gz"
tar -xOf "$cache/APKINDEX.tar.gz" APKINDEX >"$cache/APKINDEX"
lts="$(apk_name "$cache/APKINDEX" linux-lts)"
bb="$(apk_name "$cache/APKINDEX" busybox)"
musl="$(apk_name "$cache/APKINDEX" musl)"
for spec in "linux-lts:${lts}" "busybox:${bb}" "musl:${musl}"; do
  pkg="${spec%%:*}"
  name="${spec#*:}"
  if [[ ! -f "$cache/${pkg}.apk" ]]; then
    log "download ${name}"
    curl -fsSL --retry 3 -A Ternvale -o "$cache/${pkg}.apk" \
      "${alpine}/main/aarch64/${name}"
  fi
done
tar -xOf "$cache/linux-lts.apk" boot/vmlinuz-lts >"$cache/vmlinuz-lts"
unpack_zboot "$cache/vmlinuz-lts" "$out/Image"

rm -rf "$out/root"
mkdir -p "$out/root/bin" "$out/root/lib"
tar -xOf "$cache/busybox.apk" bin/busybox >"$out/root/bin/busybox"
tar -xOf "$cache/musl.apk" lib/ld-musl-aarch64.so.1 >"$out/root/lib/ld-musl-aarch64.so.1"
chmod 755 "$out/root/bin/busybox" "$out/root/lib/ld-musl-aarch64.so.1"
ln -sf ld-musl-aarch64.so.1 "$out/root/lib/libc.musl-aarch64.so.1"
cat >"$out/root/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
echo CPUINFO_BEGIN
/bin/busybox cat /proc/cpuinfo
echo CPUINFO_END
echo CPUINFO_DONE
if /bin/busybox grep -q ternvale.stay=1 /proc/cmdline; then
  while true; do /bin/busybox sleep 30; done
fi
/bin/busybox poweroff -f
INIT
chmod 755 "$out/root/init"
(cd "$out/root" && find . -mindepth 1 | sed 's|^\./||' | sort | cpio -o -H newc) \
  >"$out/initramfs.cpio" 2>/dev/null

write_toml() {
  local tag=$1 profile=$2 stay=$3
  cat >"$out/${tag}.toml" <<EOF
name = "${tag}"
backend = "native"
profile = "linux"
cpus = 1
ram_mib = 512
kernel = "$out/Image"
initrd = "$out/initramfs.cpio"
cmdline = "console=ttyAMA0 rdinit=/init${stay}"
serial_log = "$out/${tag}-serial.log"
cpu_profile = "${profile}"
[tpm]
enabled = false
[audio]
enabled = false
[usb]
enabled = false
EOF
}

boot_print() {
  local tag=$1 profile=$2
  local serial="$out/${tag}-serial.log"
  local stderr="$out/${tag}.stderr"
  write_toml "$tag" "$profile" ""
  : >"$serial"
  log "boot ${tag} cpu_profile=${profile}"
  "$root/scripts/sign-and-run.sh" "$bin" run "$out/${tag}.toml" \
    </dev/null >"$out/${tag}.stdout" 2>"$stderr" &
  local pid=$!
  if ! wait_serial "$serial" "CPUINFO_DONE" 180; then
    log "serial ${tag}:"; tail -40 "$serial" >&2 || true
    tail -20 "$stderr" >&2 || true
    fail "${tag}: no CPUINFO_DONE"
  fi
  TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$tag" --force \
    >/dev/null 2>&1 || true
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  python3 - "$serial" "$out/${tag}-features.txt" <<'PY'
import pathlib, sys
text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace")
start = text.find("CPUINFO_BEGIN")
end = text.find("CPUINFO_END")
if start < 0 or end < 0:
    sys.exit("cpuinfo markers missing")
block = text[start:end]
feats = []
for line in block.splitlines():
    if line.lower().startswith("features"):
        _, _, rest = line.partition(":")
        feats.extend(rest.split())
pathlib.Path(sys.argv[2]).write_text(" ".join(sorted(set(feats))) + "\n")
print(len(set(feats)))
PY
}

boot_print cpu-host host
boot_print cpu-base baseline

python3 - "$out" <<'PY'
import json, pathlib, sys
out = pathlib.Path(sys.argv[1])
probe = json.loads((out / "probe-cpu.json").read_text())
regs = {k: v for k, v in probe.get("registers", {}).items() if v}
host_feats = set((out / "cpu-host-features.txt").read_text().split())
base_feats = set((out / "cpu-base-features.txt").read_text().split())
summary = out / "compare.txt"

def nibble(val, n):
    return (int(val, 16) >> (n * 4)) & 0xF

def present(n):
    return n not in (0, 0xF)

# Conservative ARM64 hwcap names from ID_AA64ISAR0_EL1 (ARM ARM DDI0487).
isar0 = regs.get("ID_AA64ISAR0_EL1")
predicted = []
if isar0:
    aes = nibble(isar0, 1)
    sha1 = nibble(isar0, 2)
    sha2 = nibble(isar0, 3)
    crc = nibble(isar0, 4)
    atom = nibble(isar0, 5)
    if present(aes):
        predicted.append("aes")
        if aes >= 2:
            predicted.append("pmull")
    if present(sha1):
        predicted.append("sha1")
    if present(sha2):
        predicted.append("sha2")
    if present(crc):
        predicted.append("crc32")
    if present(atom):
        predicted.append("atomics")
pfr0 = regs.get("ID_AA64PFR0_EL1")
if pfr0 and present(nibble(pfr0, 8)):
    predicted.append("sve")
pfr1 = regs.get("ID_AA64PFR1_EL1")
if pfr1 and present(nibble(pfr1, 6)):
    predicted.append("sme")

missing = [n for n in predicted if n not in host_feats]
lines = [
    f"host_chip={probe.get('chip_brand')} {probe.get('chip_model')}",
    f"host_regs={len(regs)}",
    f"host_features={len(host_feats)} {sorted(host_feats)}",
    f"baseline_features={len(base_feats)} {sorted(base_feats)}",
    f"predicted_from_ids={predicted}",
    f"predicted_missing_in_guest={missing}",
    f"features_dropped_by_baseline={sorted(host_feats - base_feats)}",
    f"features_added_by_baseline={sorted(base_feats - host_feats)}",
]
summary.write_text("\n".join(lines) + "\n")
print("\n".join(lines))
if missing:
    sys.exit(f"guest /proc/cpuinfo missing predicted features: {missing}")
if not host_feats:
    sys.exit("guest printed no Features tokens")
PY

host_f="$out/cpu-host-features.txt"
base_f="$out/cpu-base-features.txt"
if [[ "$(cat "$host_f")" != "$(cat "$base_f")" ]]; then
  log "baseline guest Features differ from host profile (masking visible in cpuinfo)"
  echo "masking_visible=cpuinfo" >>"$out/compare.txt"
else
  log "WARN: baseline Features match host; HV may have rejected ID masks (see stderr)"
  echo "masking_visible=no_cpuinfo_delta" >>"$out/compare.txt"
  grep -E "masked ID register|rejected ID register" "$out/cpu-base.stderr" \
    | tee "$out/baseline-mask-log.txt" >&2 || true
fi

log "snapshot restore with injected richer IDs"
write_toml cpu-snap host " ternvale.stay=1"
: >"$out/cpu-snap-serial.log"
"$root/scripts/sign-and-run.sh" "$bin" run "$out/cpu-snap.toml" \
  </dev/null >"$out/cpu-snap.stdout" 2>"$out/cpu-snap.stderr" &
snap_pid=$!
if ! wait_serial "$out/cpu-snap-serial.log" "CPUINFO_DONE" 180; then
  tail -40 "$out/cpu-snap-serial.log" >&2 || true
  fail "cpu-snap: no CPUINFO_DONE"
fi
sleep 2
"$root/scripts/sign-and-run.sh" "$bin" snapshot create cpu-snap injected \
  >"$out/snap-create.out" 2>"$out/snap-create.err"
meta="$snap_dir/cpu-snap/injected/meta.json"
[[ -f "$meta" ]] || fail "missing $meta"
cp "$meta" "$out/meta-before.json"
python3 - "$meta" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
meta = json.loads(path.read_text())
ids = meta.get("id_registers") or {}
raw = ids.get("ID_AA64ISAR0_EL1") or "0x0"
val = int(raw, 16)
# Inject a present nibble 0 the real host does not advertise (0 / 0xF = absent).
injected = val | 0x2
ids["ID_AA64ISAR0_EL1"] = hex(injected)
meta["id_registers"] = ids
path.write_text(json.dumps(meta, indent=2) + "\n")
print(f"injected ID_AA64ISAR0_EL1 {raw} -> {hex(injected)}")
PY
cp "$meta" "$out/meta-injected.json"

set +e
"$root/scripts/sign-and-run.sh" "$bin" snapshot restore cpu-snap injected \
  >"$out/restore-refuse.out" 2>"$out/restore-refuse.err"
refuse_rc=$?
set -e
if [[ "$refuse_rc" -eq 0 ]]; then
  fail "restore without --force succeeded after ID injection"
fi
grep -q "ID_AA64ISAR0_EL1" "$out/restore-refuse.err" "$out/restore-refuse.out" \
  || fail "refuse error did not list ID_AA64ISAR0_EL1"
grep -q "refusing snapshot restore" "$out/restore-refuse.err" "$out/restore-refuse.out" \
  || fail "refuse error missing refusal text"
log "restore without --force refused as expected"

"$root/scripts/sign-and-run.sh" "$bin" snapshot restore cpu-snap injected --force \
  >"$out/restore-force.out" 2>"$out/restore-force.err"
grep -q "CPU feature mismatch" "$out/restore-force.err" "$out/cpu-snap.stderr" \
  || grep -q "despite CPU feature mismatch" "$out/restore-force.err" "$out/cpu-snap.stderr" \
  || fail "--force restore did not log a CPU mismatch warning"
log "--force restore proceeded with warning"

TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop cpu-snap --force \
  >/dev/null 2>&1 || true
kill "$snap_pid" 2>/dev/null || true
wait "$snap_pid" 2>/dev/null || true

cat >"$out/summary.txt" <<EOF
artifacts=${out}
$(cat "$out/compare.txt")
refuse_rc=${refuse_rc}
force=ok
EOF
log "summary:"; cat "$out/summary.txt" >&2
log "ok"
