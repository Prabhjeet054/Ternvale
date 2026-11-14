#!/usr/bin/env bash
# Step 98 live: native backend FEX — static x86_64, amd64 image payload,
# threaded x86, status engine, CPU bench vs ARM and VZ Rosetta.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
stamp="$(date +%Y%m%d-%H%M%S)"
out="$root/target/native-fex/${stamp}"
run_dir="/tmp/tvfex-$$"
bin="$target/debug/ternvale"
assets="$root/test-assets/virtio-devices"
cache="$root/target/native-fex-cache"
log() { printf 'native-fex-check: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

mkdir -p "$out" "$run_dir" "$cache"
export TERNVALE_RUN_DIR="$run_dir"
export TERNVALE_SKIP_SANDBOX=1
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::compat=debug,ternvale::backend=info,ternvale::cli=info}"

cleanup() {
  for n in native-fex vz-fex-bench; do
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$n" --force \
      >/dev/null 2>&1 || true
  done
  rm -rf "$run_dir"
}
trap cleanup EXIT

[[ -f "$assets/Image" && -f "$assets/initramfs.cpio" ]] || fail "missing $assets (make-rootfs.sh)"
command -v docker >/dev/null || fail "docker required"
docker info >/dev/null 2>&1 || fail "docker is not reachable"

(cd "$root" && cargo build -p ternvale-cli -q)
"$root/scripts/sign-and-run.sh" "$bin" --help >/dev/null

log "compile probes (x86_64 + aarch64) and extract amd64 hello-world"
mkdir -p "$out/x86"
cp "$root/guest-tests/rosetta-hello.c" "$root/guest-tests/rosetta-mt.c" \
  "$root/guest-tests/x86-bench.c" "$out/x86/"
docker run --rm --platform linux/amd64 -v "$out/x86:/build" alpine:3.20 sh -c '
set -e
apk add --no-cache gcc musl-dev >/dev/null
gcc -static -Os -Wall -Werror -o /build/hello /build/rosetta-hello.c
gcc -static -Os -Wall -Werror -pthread -o /build/mt /build/rosetta-mt.c
gcc -static -Os -Wall -Werror -o /build/bench-x86 /build/x86-bench.c
'
docker run --rm --platform linux/arm64 -v "$out/x86:/build" alpine:3.20 sh -c '
set -e
apk add --no-cache gcc musl-dev >/dev/null
gcc -static -Os -Wall -Werror -o /build/bench-arm /build/x86-bench.c
'
docker pull --platform linux/amd64 hello-world:latest >/dev/null
docker rm -f tvfex-hw >/dev/null 2>&1 || true
hw=$(docker create --platform linux/amd64 --name tvfex-hw hello-world:latest)
docker cp "$hw":/hello "$out/x86/docker-hello"
docker rm "$hw" >/dev/null
chmod 755 "$out/x86/"*
python3 - "$out/x86/hello" "$out/x86/mt" "$out/x86/bench-x86" "$out/x86/docker-hello" <<'PY' || fail "x86 probes"
import pathlib, sys
for p in sys.argv[1:]:
    b = pathlib.Path(p).read_bytes()
    if b[0:4] != b"\x7fELF" or b[4] != 2 or b[18:20] != b"\x3e\x00":
        sys.exit(f"{p} is not ELF64 x86_64")
    print(p, "ELF64 x86_64", len(b))
PY

log "FEX aarch64 userspace (Ubuntu PPA; MIT; not compiled on the Mac host)"
tar_fex="$cache/ubuntu-fex.tar"
if [[ ! -s "$tar_fex" ]]; then
  docker rm -f tvfex-ppa >/dev/null 2>&1 || true
  docker run --name tvfex-ppa --platform linux/arm64 ubuntu:24.04 bash -c '
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq software-properties-common ca-certificates gnupg >/dev/null
add-apt-repository -y ppa:fex-emu/fex >/dev/null
apt-get update -qq
if ! apt-get install -y -qq --no-install-recommends fex-emu-armv8.0; then
  apt-get install -y -qq --no-install-recommends fex-emu-armv8.4
fi
command -v FEXInterpreter >/dev/null || command -v FEX >/dev/null
'
  docker export tvfex-ppa >"$tar_fex"
  docker rm tvfex-ppa >/dev/null
fi
ls -lh "$tar_fex"

log "binfmt_misc.ko for alpine virt Image"
if [[ ! -f "$cache/binfmt_misc.ko" ]]; then
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

log "RootFS download (consent=yes; Ubuntu/Canonical; never git-bundled)"
rootfs="$cache/Ubuntu_24_04.sqsh"
if [[ ! -s "$rootfs" ]]; then
  set +e
  printf 'yes\n' | "$root/scripts/sign-and-run.sh" "$bin" x86 fetch-rootfs \
    --distro Ubuntu_24_04 --dest "$rootfs" \
    >"$out/fetch-rootfs.out" 2>"$out/fetch-rootfs.err"
  fetch_rc=$?
  set -e
  if [[ "$fetch_rc" -ne 0 ]]; then
    log "RootFS fetch failed (rc=$fetch_rc); static probes may still work"
    cat "$out/fetch-rootfs.err" >>"$out/fetch-rootfs.log" || true
  fi
else
  log "using cached $rootfs"
fi

log "initramfs with FEX + musl probes (no Ubuntu root mount)"
mkdir -p "$out/initrd"
(cd "$out/initrd" && cpio -id --quiet <"$assets/initramfs.cpio")
docker run --rm --platform linux/arm64 \
  -v "$out/initrd:/out" \
  -v "$tar_fex:/in.tar:ro" \
  ubuntu:24.04 bash -c '
set -euo pipefail
tar -xf /in.tar usr/bin/FEX usr/bin/FEXServer usr/lib/aarch64-linux-gnu/fex-emu usr/share/fex-emu
mkdir -p /out/usr/bin /out/lib/aarch64-linux-gnu /out/usr/lib/aarch64-linux-gnu /out/usr/share /out/tmp /out/root
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
mkdir -p "$out/initrd/x86" "$out/initrd/modules" "$out/initrd/usr/local/lib/ternvale/fex"
cp "$out/x86/hello" "$out/x86/mt" "$out/x86/bench-x86" "$out/x86/bench-arm" \
  "$out/x86/docker-hello" "$out/initrd/x86/"
chmod 755 "$out/initrd/x86/"*
cp "$cache/binfmt_misc.ko" "$out/initrd/modules/binfmt_misc.ko"
cp "$root/guest-tools/fex/register-binfmt.sh" "$out/initrd/usr/local/lib/ternvale/fex/"
cp "$root/guest-tools/fex/live-init.sh" "$out/initrd/fexlive.sh"
chmod 755 "$out/initrd/fexlive.sh"
python3 - "$out/initrd/init" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
text = p.read_text()
needle = "exec setsid sh -c 'exec sh -i'"
hook = """if /bin/busybox grep -q 'ternvale.fexlive=1' /proc/cmdline; then
  /bin/busybox sh /fexlive.sh
fi
exec setsid sh -c 'exec sh -i'"""
if needle not in text:
    sys.exit("virtio-devices init has no interactive shell exec to hook")
p.write_text(text.replace(needle, hook, 1))
PY
(cd "$out/initrd" && find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc) \
  >"$out/initramfs.cpio" 2>/dev/null

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

log "native boot x86_emulation=fex"
cat >"$out/native-fex.toml" <<EOF
name = "native-fex"
backend = "native"
profile = "linux"
x86_emulation = "fex"
cpus = 2
ram_mib = 1024
kernel = "$assets/Image"
initrd = "$out/initramfs.cpio"
cmdline = "console=ttyAMA0 rdinit=/init ternvale.fexlive=1"
serial_log = "$out/native-serial.log"
[tpm]
enabled = false
[audio]
enabled = false
[usb]
enabled = false
EOF
: >"$out/native-serial.log"
"$root/scripts/sign-and-run.sh" "$bin" run "$out/native-fex.toml" \
  </dev/null >"$out/native.stdout" 2>"$out/native.stderr" &
npid=$!
if ! wait_serial "$out/native-serial.log" "FEX_LIVE_START" 180; then
  fail "native: no FEX_LIVE_START (see $out/native.stderr $out/native-serial.log)"
fi
sleep 2
set +e
TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" --json status native-fex \
  >"$out/status.json" 2>"$out/status.err"
st_rc=$?
set -e
[[ "$st_rc" -eq 0 ]] || fail "status failed: $(cat "$out/status.err")"
python3 - "$out/status.json" <<'PY' || fail "status x86_engine"
import json, sys
raw = open(sys.argv[1], encoding="utf-8").read()
data = json.loads(raw)
st = data.get("status") or data
engine = st.get("x86_engine")
print("status x86_engine", engine)
if engine != "fex":
    sys.exit(f"expected fex, got {engine!r}")
PY
if ! wait_serial "$out/native-serial.log" "FEX_LIVE_DONE" 180; then
  fail "native: no FEX_LIVE_DONE"
fi
TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop native-fex --force >/dev/null 2>&1 || true
kill "$npid" 2>/dev/null || true
wait "$npid" 2>/dev/null || true

python3 - "$out/native-serial.log" "$out/native-score.txt" <<'PY' || fail "native serial checks"
from pathlib import Path
import sys
text = Path(sys.argv[1]).read_text(errors="replace")
need = [
    "FEX_INTERPRETER_OK",
    "x86_hello",
    "DIRECT_HELLO_RC=0",
    "mt_x86 n=4",
    "DIRECT_MT_RC=0",
    "DOCKER_IMAGE_RC=0",
    "ARM_BENCH_RC=0",
    "FEX_BENCH_RC=0",
    "bench_ns=",
]
rows = []
ok = True
for n in need:
    hit = n in text
    rows.append(f"| {n} | {'pass' if hit else 'FAIL'} |")
    ok = ok and hit
Path(sys.argv[2]).write_text("\n".join(rows) + "\n")
print("\n".join(rows))
if "DOCKER_SKIP" not in text:
    print("unexpected: dockerd present")
if not ok:
    sys.exit(1)
PY

log "VZ Rosetta bench (same x86 binary)"
mkdir -p "$out/vzinit"
(cd "$out/vzinit" && cpio -id --quiet <"$root/test-assets/virtio-devices/initramfs.cpio")
mkdir -p "$out/vzinit/x86"
cp "$out/x86/bench-x86" "$out/vzinit/x86/bench-x86"
chmod 755 "$out/vzinit/x86/bench-x86"
if [[ -f "$cache/binfmt_misc.ko" ]]; then
  mkdir -p "$out/vzinit/modules"
  cp "$cache/binfmt_misc.ko" "$out/vzinit/modules/binfmt_misc.ko"
fi
cat >"$out/vzinit/rosettabench.sh" <<'GUEST'
#!/bin/busybox sh
echo ROSETTA_BENCH_START
/bin/busybox mkdir -p /mnt/rosetta /proc/sys/fs/binfmt_misc
/bin/busybox mount -t virtiofs rosetta /mnt/rosetta || echo ROSETTA_MOUNT_FAIL
RUNTIME=/mnt/rosetta/rosetta
if [ -d "$RUNTIME" ] && [ -e "$RUNTIME/rosetta" ]; then RUNTIME="$RUNTIME/rosetta"; fi
/bin/busybox insmod /modules/binfmt_misc.ko 2>/dev/null || true
/bin/busybox mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc 2>/dev/null || true
if [ -f "$RUNTIME" ]; then
  "$RUNTIME" /x86/bench-x86
  echo ROSETTA_BENCH_RC=$?
fi
echo ROSETTA_BENCH_DONE
GUEST
chmod 755 "$out/vzinit/rosettabench.sh"
python3 - "$out/vzinit/init" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
text = p.read_text()
needle = "exec setsid sh -c 'exec sh -i'"
hook = """if /bin/busybox grep -q 'ternvale.rosettabench=1' /proc/cmdline; then
  /bin/busybox sh /rosettabench.sh
fi
exec setsid sh -c 'exec sh -i'"""
if needle not in text:
    sys.exit("vz init hook missing")
p.write_text(text.replace(needle, hook, 1))
PY
(cd "$out/vzinit" && find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc) \
  >"$out/vz-initramfs.cpio" 2>/dev/null
cat >"$out/vz-fex-bench.toml" <<EOF
name = "vz-fex-bench"
backend = "vz"
profile = "linux"
rosetta = true
cpus = 2
ram_mib = 512
kernel = "$root/test-assets/virtio-devices/Image"
initrd = "$out/vz-initramfs.cpio"
cmdline = "console=hvc0 rdinit=/init ternvale.rosettabench=1"
serial_log = "$out/vz-serial.log"
[tpm]
enabled = false
EOF
: >"$out/vz-serial.log"
"$root/scripts/sign-and-run.sh" "$bin" run "$out/vz-fex-bench.toml" \
  </dev/null >"$out/vz.stdout" 2>"$out/vz.stderr" &
vpid=$!
if ! wait_serial "$out/vz-serial.log" "ROSETTA_BENCH_DONE" 120; then
  log "WARN: VZ Rosetta bench did not finish (see $out/vz-serial.log)"
else
  TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" --json status vz-fex-bench \
    >"$out/vz-status.json" 2>/dev/null || true
fi
TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop vz-fex-bench --force >/dev/null 2>&1 || true
kill "$vpid" 2>/dev/null || true
wait "$vpid" 2>/dev/null || true

python3 - "$out" "$root/docs/X86_COMPAT.md" <<'PY'
import pathlib, re, json, sys
out = pathlib.Path(sys.argv[1])
dest = pathlib.Path(sys.argv[2])
native = (out / "native-serial.log").read_text(errors="replace")
vz = (out / "vz-serial.log").read_text(errors="replace") if (out / "vz-serial.log").exists() else ""
def first(pat, text):
    m = re.search(pat, text)
    return m.group(1) if m else "n/a"
arm = first(r"ARM_BENCH_RC=0[\s\S]*?bench_ns=(\d+)", native)
if arm == "n/a":
    arm = first(r"bench_ns=(\d+)[\s\S]*?ARM_BENCH_RC=0", native)
# Take bench_ns lines in order: ARM then FEX (guest runs ARM then FEX).
ns = re.findall(r"bench_ns=(\d+)", native)
arm_ns = ns[0] if ns else "n/a"
fex_ns = ns[1] if len(ns) > 1 else "n/a"
ros_ns = first(r"bench_ns=(\d+)", vz)
engine = "unknown"
if (out / "status.json").exists():
    data = json.loads((out / "status.json").read_text())
    st = data.get("status") or data
    engine = st.get("x86_engine") or engine
vz_engine = ""
if (out / "vz-status.json").exists():
    try:
        data = json.loads((out / "vz-status.json").read_text())
        st = data.get("status") or data
        vz_engine = st.get("x86_engine") or ""
    except json.JSONDecodeError:
        pass
fails = []
if "DOCKER_SKIP" in native:
    fails.append("| `docker run --platform linux/amd64` in guest | skipped | No dockerd in the busybox initramfs guest. The hello-world amd64 payload was run through FEX (`DOCKER_IMAGE_RC`). |")
if "FEX_ROOTFS_ABSENT" in native:
    fails.append("| Dynamic glibc x86_64 needing FEX RootFS | not exercised | Host may have fetched the catalog squashfs; it is not unpacked into the initramfs. Static musl probes do not need it. |")
if "DIRECT_HELLO_RC=0" not in native:
    fails.append("| static `hello` via FEX | FAIL | See `native-serial.log`. |")
if "DIRECT_MT_RC=0" not in native:
    fails.append("| threaded `mt` via FEX | FAIL | See `native-serial.log`. |")
if "DOCKER_IMAGE_RC=0" not in native:
    fails.append("| amd64 hello-world payload | FAIL | See `native-serial.log`. |")
if "ROSETTA_BENCH_RC=0" not in vz:
    fails.append("| Rosetta CPU bench (VZ) | incomplete | Share/binfmt/bench did not print `ROSETTA_BENCH_RC=0`. |")
if not fails:
    fails.append("| — | — | No additional failures in this run. |")
body = f"""# x86 compatibility (Step 98 live)

Host date in artifacts: `{out.name}`.
Native status `x86_engine`: **{engine}**. VZ status `x86_engine`: **{vz_engine or "n/a"}**.

## What ran (native + FEX)

| Probe | Result |
| --- | --- |
| static musl x86_64 `hello` | {"pass" if "DIRECT_HELLO_RC=0" in native else "FAIL"} |
| threaded musl x86_64 `mt` (4 pthreads) | {"pass" if "DIRECT_MT_RC=0" in native else "FAIL"} |
| amd64 Docker *image binary* (`hello-world` `/hello`) | {"pass" if "DOCKER_IMAGE_RC=0" in native else "FAIL"} |
| `docker run` amd64 in guest | skipped (no dockerd) |
| native ARM `bench-arm` | {"pass" if "ARM_BENCH_RC=0" in native else "FAIL"} |
| FEX `bench-x86` | {"pass" if "FEX_BENCH_RC=0" in native else "FAIL"} |
| VZ Rosetta `bench-x86` | {"pass" if "ROSETTA_BENCH_RC=0" in vz else "FAIL"} |

## CPU benchmark (`guest-tests/x86-bench.c`, 80e6 iters)

Same source, static musl. Wall time from `CLOCK_MONOTONIC` inside the guest.

| Engine | backend | `bench_ns` | vs ARM |
| --- | --- | ---: | ---: |
| native ARM64 | native | {arm_ns} | 1.0x |
| FEX (x86_64) | native | {fex_ns} | {"n/a" if arm_ns == "n/a" or fex_ns == "n/a" else f"{int(fex_ns)/int(arm_ns):.2f}x"} |
| Rosetta (x86_64) | vz | {ros_ns} | {"n/a" if arm_ns == "n/a" or ros_ns == "n/a" else f"{int(ros_ns)/int(arm_ns):.2f}x"} |

Lower `bench_ns` is faster. These are not SPECint; they only compare this loop
on this Mac. FEX JIT vs Apple’s Linux Rosetta is expected to differ.

## Programs that failed (or were skipped) and why

| Program | Outcome | Why |
| --- | --- | --- |
{chr(10).join(fails)}

## Notes

- Native Hypervisor.framework still cannot attach Apple Rosetta.
- FEX came from the Ubuntu PPA inside an aarch64 userspace disk (MIT), not from
  extracting Rosetta. RootFS squashfs is Canonical’s and is not in git.
- Artifacts: `{out}`.
"""
dest.write_text(body)
print(body)
PY

log "PASS"
log "artifacts $out"
echo "$out" >"$root/target/native-fex/latest"
