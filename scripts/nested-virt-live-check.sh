#!/usr/bin/env bash
# Step 102 live: Linux nested_virt=true vs false, EL2/KVM dmesg, optional L2 lkvm.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
stamp="$(date +%Y%m%d-%H%M%S)"
out="$root/target/nested-virt/${stamp}"
cache="$root/target/nested-virt-cache"
run_dir="/tmp/tvnested-$$"
bin="$target/debug/ternvale"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"
log() { printf 'nested-virt-live: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

mkdir -p "$out" "$cache" "$run_dir"
export TERNVALE_RUN_DIR="$run_dir"
export TERNVALE_SKIP_SANDBOX=1
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::nested=debug,ternvale::boot=info}"
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-$root/entitlements/ternvale-hv-only.entitlements}"

cleanup() {
  for n in nested-el2 nested-el1; do
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$n" --force \
      >/dev/null 2>&1 || true
  done
  rm -rf "$run_dir"
}
trap cleanup EXIT

command -v docker >/dev/null || fail "docker required"
docker info >/dev/null 2>&1 || fail "docker is not reachable"
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
print(len(image), file=sys.stderr)
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
log "unpack Alpine linux-lts Image (EL2 probe; this kernel has no kvm.ko)"
tar -xOf "$cache/linux-lts.apk" boot/vmlinuz-lts >"$cache/vmlinuz-lts"
unpack_zboot "$cache/vmlinuz-lts" "$out/Image-alpine"
: >"$cache/kvm-order"

log "L2 micro initramfs"
mkdir -p "$out/l2root/bin"
tar -xOf "$cache/busybox.apk" bin/busybox >"$out/l2root/bin/busybox"
chmod 755 "$out/l2root/bin/busybox"
cat >"$out/l2root/init" <<'L2'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
echo L2_BOOTED
/bin/busybox poweroff -f
L2
chmod 755 "$out/l2root/init"
(cd "$out/l2root" && find . -mindepth 1 | sed 's|^\./||' | sort | cpio -o -H newc) \
  >"$out/l2.cpio" 2>/dev/null

log "L1 initramfs"
rm -rf "$out/l1"
mkdir -p "$out/l1/bin" "$out/l1/lib" "$out/l1/modules" "$out/l1/l2"
tar -xOf "$cache/busybox.apk" bin/busybox >"$out/l1/bin/busybox"
tar -xOf "$cache/musl.apk" lib/ld-musl-aarch64.so.1 >"$out/l1/lib/ld-musl-aarch64.so.1"
chmod 755 "$out/l1/bin/busybox" "$out/l1/lib/ld-musl-aarch64.so.1"
ln -sf ld-musl-aarch64.so.1 "$out/l1/lib/libc.musl-aarch64.so.1"
if [[ -x "$cache/lkvm" ]]; then
  cp "$cache/lkvm" "$out/l1/bin/lkvm"
  chmod 755 "$out/l1/bin/lkvm"
fi
cp "$out/Image-alpine" "$out/l1/l2/Image"
cp "$out/l2.cpio" "$out/l1/l2/initrd"
cp "$root/guest-tools/nested/live-init.sh" "$out/l1/nestedlive.sh"
chmod 755 "$out/l1/nestedlive.sh"
: >"$out/l1/modules/order"
if [[ -s "$cache/kvm-order" ]]; then
  while read -r base; do
    [[ -n "$base" ]] || continue
    [[ -f "$cache/kvm-ko/${base}.ko" ]] || fail "missing $cache/kvm-ko/${base}.ko"
    cp "$cache/kvm-ko/${base}.ko" "$out/l1/modules/${base}.ko"
    printf '%s\n' "$base" >>"$out/l1/modules/order"
  done <"$cache/kvm-order"
fi
cat >"$out/l1/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /sbin /usr/bin /usr/sbin
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
/bin/busybox --install -s
if /bin/busybox grep -q ternvale.nested=1 /proc/cmdline; then
  /bin/busybox sh /nestedlive.sh
  exit 0
fi
exec setsid sh -c 'exec sh -i' </dev/console >/dev/console 2>&1
INIT
chmod 755 "$out/l1/init"
(cd "$out/l1" && find . -mindepth 1 | sed 's|^\./||' | sort | cpio -o -H newc) \
  >"$out/l1.cpio" 2>/dev/null

boot_one() {
  local tag=$1 nested=$2 kernel=$3
  local must=${4:-must}
  local serial="$out/${tag}-serial.log"
  local stderr="$out/${tag}.stderr"
  local stdout="$out/${tag}.stdout"
  local toml="$out/${tag}.toml"
  cat >"$toml" <<EOF
name = "${tag}"
backend = "native"
profile = "linux"
cpus = 2
ram_mib = 1024
kernel = "${kernel}"
initrd = "$out/l1.cpio"
cmdline = "console=ttyAMA0 rdinit=/init ternvale.nested=1"
serial_log = "${serial}"
nested_virt = ${nested}
[tpm]
enabled = false
[audio]
enabled = false
[usb]
enabled = false
EOF
  : >"$serial"
  log "boot ${tag} nested_virt=${nested} kernel=${kernel}"
  local t0 t1 ms cpu
  t0=$(python3 -c 'import time; print(time.time())')
  "$root/scripts/sign-and-run.sh" "$bin" run "$toml" </dev/null >"$stdout" 2>"$stderr" &
  local pid=$!
  local timeout=90
  [[ "$nested" = true ]] && timeout=12
  if wait_serial "$serial" "NESTED_PROBE_DONE" "$timeout"; then
    :
  elif [[ "$nested" = true ]] && grep -q "unimplemented EL2 sysreg trap" "$stderr" 2>/dev/null; then
    log "${tag}: guest hung on unimplemented EL2 sysreg traps (no userspace)"
  else
    log "serial ${tag}:"; tail -40 "$serial" >&2 || true
    log "stderr ${tag}:"; tail -40 "$stderr" >&2 || true
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$tag" --force >/dev/null 2>&1 || true
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    if [[ "$must" = must ]]; then
      fail "${tag}: no NESTED_PROBE_DONE"
    fi
    log "WARN: ${tag} did not finish probe"
    echo "timeout" >"$out/${tag}.boot_ms"
    echo "?" >"$out/${tag}.host_cpu"
    return 0
  fi
  t1=$(python3 -c 'import time; print(time.time())')
  ms=$(python3 -c "print(int((${t1}-${t0})*1000))")
  cpu=$(ps -p "$pid" -o %cpu= 2>/dev/null | awk '{print $1}' || echo "?")
  echo "${ms}" >"$out/${tag}.boot_ms"
  echo "${cpu}" >"$out/${tag}.host_cpu"
  log "${tag} boot_ms=${ms} host_cpu_pct=${cpu}"
  TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$tag" --force >/dev/null 2>&1 || true
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

boot_one nested-el1 false "$out/Image-alpine" must
boot_one nested-el2 true "$out/Image-alpine" optional

if grep -q "NESTED_PROBE_DONE" "$out/nested-el2-serial.log" 2>/dev/null; then
  log "nested_virt=true reached userspace; KVM/L2 would run here"
else
  log "skip Ubuntu/KVM L2 boot; L1 nested_virt=true did not reach init"
fi

el2=$(grep -E "CPU: All CPU" "$out/nested-el2-serial.log" 2>/dev/null || true)
el1=$(grep -E "CPU: All CPU" "$out/nested-el1-serial.log" 2>/dev/null || true)
log "el2 line: ${el2:-missing}"
log "el1 line: ${el1:-missing}"
grep -q "NESTED_PROBE_DONE" "$out/nested-el1-serial.log" || fail "nested_virt=false did not finish the L1 probe"
if echo "$el1" | grep -q "EL1"; then
  log "nested_virt=false started at EL1"
else
  log "WARN: nested_virt=false had no 'started at EL1' line (kernel may use another wording)"
fi
traps=$(grep -c "unimplemented EL2 sysreg trap" "$out/nested-el2.stderr" 2>/dev/null || true)
traps=${traps:-0}
cpsr=$(grep "boot cpu CPSR" "$out/nested-el2.stderr" | tail -1 || true)
log "nested_virt=true host: ${cpsr:-no cpsr log} traps=${traps}"
[[ "$traps" -gt 0 ]] || grep -q "el2=true" "$out/nested-el2.stderr" || fail "nested_virt=true did not enable EL2"

if grep -q "DEV_KVM=yes" "$out/nested-el1-serial.log"; then
  fail "nested_virt=false unexpectedly has /dev/kvm"
fi
if ! grep -q "NESTED_PROBE_DONE" "$out/nested-el2-serial.log" 2>/dev/null; then
  log "KVM/L2 skipped: nested_virt=true never reached userspace (EL2 traps)"
fi

el2_ms=$(cat "$out/nested-el2.boot_ms")
el1_ms=$(cat "$out/nested-el1.boot_ms")
el2_cpu=$(cat "$out/nested-el2.host_cpu")
el1_cpu=$(cat "$out/nested-el1.host_cpu")
overhead=n/a
if grep -q "NESTED_PROBE_DONE" "$out/nested-el2-serial.log" 2>/dev/null; then
  overhead=$(python3 -c "a='${el2_ms}'; b='${el1_ms}'
print('n/a' if not a.isdigit() or not b.isdigit() or int(b)==0 else '{:.1f}'.format((int(a)-int(b))*100.0/int(b)))")
fi
cat >"$out/summary.txt" <<EOF
nested_virt=true  boot_ms=${el2_ms} host_cpu_pct=${el2_cpu}
nested_virt=false boot_ms=${el1_ms} host_cpu_pct=${el1_cpu}
l1_boot_overhead_pct=${overhead}
el2_line=${el2}
el1_line=${el1}
kvm_el2=$(grep DEV_KVM= "$out/nested-el2-serial.log" | tail -1)
kvm_el1=$(grep DEV_KVM= "$out/nested-el1-serial.log" | tail -1)
l2=$(grep -E 'L2_' "$out/nested-el2-serial.log" "$out/nested-kvm-serial.log" 2>/dev/null | tr '\n' ' ')
ubuntu_el=$(grep -E "CPU: All CPU" "$out/nested-kvm-serial.log" 2>/dev/null || true)
kvm_ubuntu=$(grep DEV_KVM= "$out/nested-kvm-serial.log" 2>/dev/null | tail -1)
artifacts=${out}
EOF
log "summary:"; cat "$out/summary.txt" >&2
log "ok"
