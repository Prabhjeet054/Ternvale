#!/usr/bin/env bash
# Step 99 live: same static x86_64 probes on Rosetta (VZ), FEX (native),
# and qemu-user-static (native, distro package in the guest — not git-bundled).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
stamp="$(date +%Y%m%d-%H%M%S)"
out="$root/target/x86-suite/${stamp}"
run_dir="/tmp/tvx86-$$"
bin="$target/debug/ternvale"
assets="$root/test-assets/virtio-devices"
cache="$root/target/x86-suite-cache"
iters="${LITMUS_ITERS:-2000000}"
log() { printf 'x86-engine-suite: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

mkdir -p "$out" "$run_dir" "$cache"
export TERNVALE_RUN_DIR="$run_dir"
export TERNVALE_SKIP_SANDBOX=1
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::compat=debug,ternvale::cli=info}"
export PYTHONPATH="$root/scripts/x86_suite${PYTHONPATH:+:$PYTHONPATH}"

cleanup() {
  for n in x86-suite-native x86-suite-vz; do
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$n" --force \
      >/dev/null 2>&1 || true
  done
  rm -rf "$run_dir"
}
trap cleanup EXIT

[[ -f "$assets/Image" && -f "$assets/initramfs.cpio" ]] || fail "missing $assets"
command -v docker >/dev/null || fail "docker required"
docker info >/dev/null 2>&1 || fail "docker is not reachable"

(cd "$root" && python3 -m unittest discover -s "$root/scripts/x86_suite" -p 'test_*.py' -v)
(cd "$root" && cargo build -p ternvale-cli -q)
"$root/scripts/sign-and-run.sh" "$bin" --help >/dev/null

log "compile static x86_64 probes (iters=$iters)"
mkdir -p "$out/x86"
cp "$root/guest-tests/x86-det.c" "$root/guest-tests/x86-litmus.c" \
  "$root/guest-tests/rosetta-hello.c" "$out/x86/"
docker run --rm --platform linux/amd64 -v "$out/x86:/build" alpine:3.20 sh -c '
set -e
apk add --no-cache gcc musl-dev >/dev/null
gcc -static -Os -Wall -Werror -o /build/det /build/x86-det.c
gcc -static -Os -Wall -Werror -pthread -o /build/litmus /build/x86-litmus.c
gcc -static -Os -Wall -Werror -o /build/hello /build/rosetta-hello.c
'
python3 - "$out/x86/det" "$out/x86/litmus" "$out/x86/hello" <<'PY' || fail "not x86_64"
import pathlib, sys
for p in sys.argv[1:]:
    b = pathlib.Path(p).read_bytes()
    if b[0:4] != b"\x7fELF" or b[4] != 2 or b[18:20] != b"\x3e\x00":
        sys.exit(f"{p} is not ELF64 x86_64")
PY

log "FEX aarch64 userspace (Ubuntu PPA cache; MIT)"
tar_fex="$cache/ubuntu-fex.tar"
if [[ ! -s "$tar_fex" ]]; then
  if [[ -s "$root/target/native-fex-cache/ubuntu-fex.tar" ]]; then
    cp "$root/target/native-fex-cache/ubuntu-fex.tar" "$tar_fex"
  else
    docker rm -f tvx86-fex >/dev/null 2>&1 || true
    docker run --name tvx86-fex --platform linux/arm64 ubuntu:24.04 bash -c '
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq software-properties-common ca-certificates gnupg >/dev/null
add-apt-repository -y ppa:fex-emu/fex >/dev/null
apt-get update -qq
if ! apt-get install -y -qq --no-install-recommends fex-emu-armv8.0; then
  apt-get install -y -qq --no-install-recommends fex-emu-armv8.4
fi
command -v FEX >/dev/null
'
    docker export tvx86-fex >"$tar_fex"
    docker rm tvx86-fex >/dev/null
  fi
fi

log "qemu-user-static from Ubuntu arm64 (GPL; test cache only, never git)"
qemu_bin="$cache/qemu-x86_64-static"
if [[ ! -x "$qemu_bin" ]]; then
  docker rm -f tvx86-qemu >/dev/null 2>&1 || true
  docker run --name tvx86-qemu --platform linux/arm64 ubuntu:24.04 bash -c '
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends qemu-user-static
test -x /usr/bin/qemu-x86_64-static
'
  docker cp tvx86-qemu:/usr/bin/qemu-x86_64-static "$qemu_bin"
  docker rm tvx86-qemu >/dev/null
  chmod 755 "$qemu_bin"
fi
python3 - "$qemu_bin" <<'PY' || fail "qemu-user-static is not aarch64"
import pathlib, sys
b = pathlib.Path(sys.argv[1]).read_bytes()
if b[0:4] != b"\x7fELF" or b[4] != 2 or b[18:20] != b"\xb7\x00":
    sys.exit("qemu-x86_64-static must be ELF64 aarch64 (host translator)")
print("qemu-user-static aarch64", len(b), "GPL not bundled")
PY

log "binfmt_misc.ko"
if [[ ! -f "$cache/binfmt_misc.ko" ]]; then
  if [[ -f "$root/target/native-fex-cache/binfmt_misc.ko" ]]; then
    cp "$root/target/native-fex-cache/binfmt_misc.ko" "$cache/binfmt_misc.ko"
  else
    alpine_ver="${ALPINE_VER:-3.20}"
    curl -fsSL --retry 3 -A Ternvale -o "$cache/APKINDEX.tar.gz" \
      "https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}/main/aarch64/APKINDEX.tar.gz"
    tar -xOf "$cache/APKINDEX.tar.gz" APKINDEX >"$cache/APKINDEX"
    virt_apk=$(python3 - "$cache/APKINDEX" <<'PY'
import sys
text = open(sys.argv[1], encoding="utf-8", errors="replace").read().split("\n\n")
for block in text:
    fields = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
    if fields.get("P") == "linux-virt":
        print(fields["P"] + "-" + fields["V"] + ".apk")
        break
else:
    raise SystemExit("linux-virt missing")
PY
    )
    curl -fsSL --retry 3 -A Ternvale -o "$cache/linux-virt.apk" \
      "https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}/main/aarch64/${virt_apk}"
    rel=$(tar -tf "$cache/linux-virt.apk" | grep -E 'binfmt_misc\.ko' | head -1) \
      || fail "no binfmt_misc.ko"
    case "$rel" in
      *.ko.gz) tar -xOf "$cache/linux-virt.apk" "$rel" | gzip -dc >"$cache/binfmt_misc.ko" ;;
      *.ko) tar -xOf "$cache/linux-virt.apk" "$rel" >"$cache/binfmt_misc.ko" ;;
      *) fail "$rel" ;;
    esac
  fi
fi

hook_init() {
  python3 - "$1" "$2" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
flag = sys.argv[2]
text = p.read_text()
needle = "exec setsid sh -c 'exec sh -i'"
hook = f"""if /bin/busybox grep -q '{flag}' /proc/cmdline; then
  /bin/busybox sh /suitelive.sh
fi
exec setsid sh -c 'exec sh -i'"""
if needle not in text:
    sys.exit("virtio-devices init has no interactive shell exec to hook")
p.write_text(text.replace(needle, hook, 1))
PY
}

wait_serial() {
  local file=$1 pattern=$2 timeout=${3:-600}
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

log "native initramfs (FEX + qemu-user-static)"
mkdir -p "$out/initrd"
(cd "$out/initrd" && cpio -id --quiet <"$assets/initramfs.cpio")
docker run --rm --platform linux/arm64 \
  -v "$out/initrd:/out" \
  -v "$tar_fex:/in.tar:ro" \
  ubuntu:24.04 bash -c '
set -euo pipefail
tar -xf /in.tar usr/bin/FEX usr/bin/FEXServer usr/lib/aarch64-linux-gnu/fex-emu usr/share/fex-emu
mkdir -p /out/usr/bin /out/lib/aarch64-linux-gnu /out/usr/lib/aarch64-linux-gnu /out/usr/share
cp -a /usr/bin/FEX /usr/bin/FEXServer /out/usr/bin/
cp -a /usr/lib/aarch64-linux-gnu/fex-emu /out/usr/lib/aarch64-linux-gnu/
cp -a /usr/share/fex-emu /out/usr/share/
cp -a /lib/aarch64-linux-gnu/libstdc++.so.6* /out/lib/aarch64-linux-gnu/
cp -a /lib/aarch64-linux-gnu/libm.so.6* /out/lib/aarch64-linux-gnu/
cp -a /lib/aarch64-linux-gnu/libgcc_s.so.1* /out/lib/aarch64-linux-gnu/
cp -a /lib/aarch64-linux-gnu/libc.so.6 /out/lib/aarch64-linux-gnu/
if [ -e /lib/aarch64-linux-gnu/ld-linux-aarch64.so.1 ]; then
  cp -a /lib/aarch64-linux-gnu/ld-linux-aarch64.so.1 /out/lib/aarch64-linux-gnu/
fi
cp -a /lib/ld-linux-aarch64.so.1 /out/lib/ 2>/dev/null || \
  ln -sf aarch64-linux-gnu/ld-linux-aarch64.so.1 /out/lib/ld-linux-aarch64.so.1
ln -sf FEX /out/usr/bin/FEXInterpreter
chmod 0755 /out/usr/bin/FEX /out/usr/bin/FEXServer /out/lib/ld-linux-aarch64.so.1 2>/dev/null || true
'
mkdir -p "$out/initrd/x86" "$out/initrd/usr/bin"
cp "$out/x86/det" "$out/x86/litmus" "$out/x86/hello" "$out/initrd/x86/"
chmod 755 "$out/initrd/x86/"*
cp "$qemu_bin" "$out/initrd/usr/bin/qemu-x86_64-static"
chmod 755 "$out/initrd/usr/bin/qemu-x86_64-static"
cp "$root/guest-tools/x86-suite/live-init.sh" "$out/initrd/suitelive.sh"
chmod 755 "$out/initrd/suitelive.sh"
hook_init "$out/initrd/init" "ternvale.x86suite=1"
(cd "$out/initrd" && find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc) \
  >"$out/native-initramfs.cpio" 2>/dev/null

cat >"$out/native.toml" <<EOF
name = "x86-suite-native"
backend = "native"
profile = "linux"
x86_emulation = "fex"
cpus = 2
ram_mib = 1024
kernel = "$assets/Image"
initrd = "$out/native-initramfs.cpio"
cmdline = "console=ttyAMA0 rdinit=/init ternvale.x86suite=1 LITMUS_ITERS=${iters}"
serial_log = "$out/native-serial.log"
[tpm]
enabled = false
[audio]
enabled = false
[usb]
enabled = false
EOF
: >"$out/native-serial.log"
log "boot native FEX + qemu-user"
"$root/scripts/sign-and-run.sh" "$bin" run "$out/native.toml" \
  </dev/null >"$out/native.stdout" 2>"$out/native.stderr" &
npid=$!
if ! wait_serial "$out/native-serial.log" "SUITE_DONE" 1800; then
  fail "native: no SUITE_DONE (see $out/native.stderr $out/native-serial.log)"
fi
set +e
TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" --json status x86-suite-native \
  >"$out/native-status.json" 2>"$out/native-status.err"
set -e
TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop x86-suite-native --force \
  >/dev/null 2>&1 || true
kill "$npid" 2>/dev/null || true
wait "$npid" 2>/dev/null || true

log "VZ Rosetta initramfs"
mkdir -p "$out/vzinit"
(cd "$out/vzinit" && cpio -id --quiet <"$assets/initramfs.cpio")
mkdir -p "$out/vzinit/x86" "$out/vzinit/modules"
cp "$out/x86/det" "$out/x86/litmus" "$out/x86/hello" "$out/vzinit/x86/"
chmod 755 "$out/vzinit/x86/"*
cp "$cache/binfmt_misc.ko" "$out/vzinit/modules/binfmt_misc.ko"
cp "$root/guest-tools/x86-suite/rosetta-init.sh" "$out/vzinit/suitelive.sh"
chmod 755 "$out/vzinit/suitelive.sh"
hook_init "$out/vzinit/init" "ternvale.x86suite=1"
(cd "$out/vzinit" && find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc) \
  >"$out/vz-initramfs.cpio" 2>/dev/null
cat >"$out/vz.toml" <<EOF
name = "x86-suite-vz"
backend = "vz"
profile = "linux"
rosetta = true
cpus = 2
ram_mib = 512
kernel = "$assets/Image"
initrd = "$out/vz-initramfs.cpio"
cmdline = "console=hvc0 rdinit=/init ternvale.x86suite=1 LITMUS_ITERS=${iters}"
serial_log = "$out/vz-serial.log"
[tpm]
enabled = false
EOF
: >"$out/vz-serial.log"
"$root/scripts/sign-and-run.sh" "$bin" run "$out/vz.toml" \
  </dev/null >"$out/vz.stdout" 2>"$out/vz.stderr" &
vpid=$!
if ! wait_serial "$out/vz-serial.log" "SUITE_DONE" 1800; then
  log "WARN: VZ suite did not finish (see $out/vz-serial.log)"
else
  TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" --json status x86-suite-vz \
    >"$out/vz-status.json" 2>/dev/null || true
fi
TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop x86-suite-vz --force \
  >/dev/null 2>&1 || true
kill "$vpid" 2>/dev/null || true
wait "$vpid" 2>/dev/null || true

python3 - "$out" "$root/docs/X86_COMPAT.md" "$iters" <<'PY'
import json, pathlib, sys
from report import EngineRun, parse_engine_block, render_compat, split_engines

out = pathlib.Path(sys.argv[1])
dest = pathlib.Path(sys.argv[2])
native = (out / "native-serial.log").read_text(errors="replace")
vz = (out / "vz-serial.log").read_text(errors="replace") if (out / "vz-serial.log").exists() else ""
runs = split_engines(native)
if "ENGINE=rosetta" in vz or "ENGINE=rosetta" in vz.replace("\r", ""):
    runs["rosetta"] = parse_engine_block("rosetta", vz)
elif "rosetta" not in runs:
    runs["rosetta"] = EngineRun(name="rosetta", notes=["no VZ serial block"])

def engine_from(path, default="n/a"):
    p = out / path
    if not p.exists():
        return default
    try:
        data = json.loads(p.read_text())
    except json.JSONDecodeError:
        return default
    st = data.get("status") or data
    return st.get("x86_engine") or default

skips = []
if "ENGINE_SKIP" in native and "qemu-user" in native:
    if runs.get("qemu-user") and not runs["qemu-user"].ran():
        skips.append("| qemu-user | skipped | qemu-x86_64-static missing in initramfs |")
if not runs.get("rosetta") or not runs["rosetta"].ran():
    skips.append("| Rosetta | skipped/incomplete | VZ share or suite did not finish |")
for name, run in runs.items():
    if run.mp_forbidden:
        skips.append(f"| {name} MP | VIOLATION | forbidden={run.mp_forbidden} (x86 TSO forbids this) |")
    if run.det_line and not run.det_ok():
        skips.append(f"| {name} det | FAIL | stdout {run.det_line!r} |")
if not skips:
    skips.append("| — | — | none |")

prev = dest.read_text() if dest.exists() else "# x86 compatibility\n"
body = render_compat(
    prev,
    out.name,
    engine_from("native-status.json"),
    engine_from("vz-status.json"),
    runs,
    str(out),
    skips,
)
dest.write_text(body)
print(body)
PY

log "PASS"
log "artifacts $out"
echo "$out" >"$root/target/x86-suite/latest"
