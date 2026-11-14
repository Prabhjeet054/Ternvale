#!/usr/bin/env bash
# Step 96 live: Linux VZ Rosetta share, binfmt, static + threaded x86_64,
# consent (no install without yes). Docker-in-guest is attempted if present.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
out="$root/target/vz-rosetta/$(date +%Y%m%d-%H%M%S)"
run_dir="/tmp/tvvz-rosetta-$$"
bin="$target/debug/ternvale"
assets="$root/test-assets/virtio-devices"
log() { printf 'vz-rosetta-check: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

mkdir -p "$out" "$run_dir"
export TERNVALE_RUN_DIR="$run_dir"
export TERNVALE_SKIP_SANDBOX=1
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::rosetta=debug,ternvale::vz=debug,ternvale::cli=info}"

(cd "$root" && cargo build -p ternvale-cli -q)
"$root/scripts/sign-and-run.sh" "$bin" --help >/dev/null

# --- consent: nothing installs without yes (simulate Apple download) ---
log "consent: decline"
set +e
printf 'no\n' | TERNVALE_ROSETTA_INSTALL_SIMULATE=1 \
  "$root/scripts/sign-and-run.sh" "$bin" doctor --install-rosetta \
  >"$out/consent-no.out" 2>"$out/consent-no.err"
no_rc=$?
set -e
cat "$out/consent-no.out" "$out/consent-no.err" | tee "$out/consent-no.log" >/dev/null
grep -q 'declined\|nothing was downloaded' "$out/consent-no.log" \
  || fail "decline did not print a declined message (rc=$no_rc): $(cat "$out/consent-no.log")"
[[ "$no_rc" -ne 0 ]] || fail "decline must be a non-zero exit"
log "consent: decline rc=$no_rc (no installRosetta)"

log "consent: simulate yes (still no Apple download)"
set +e
printf 'yes\n' | TERNVALE_ROSETTA_INSTALL_SIMULATE=1 \
  "$root/scripts/sign-and-run.sh" "$bin" doctor --install-rosetta \
  >"$out/consent-yes.out" 2>"$out/consent-yes.err"
yes_rc=$?
set -e
[[ "$yes_rc" -eq 0 ]] || fail "simulated yes should succeed: $(cat "$out/consent-yes.err")"
if grep -q 'calling VZLinuxRosettaDirectoryShare.installRosetta' "$out/consent-yes.err"; then
  fail "simulate yes still called installRosetta"
fi
log "consent: simulated yes rc=$yes_rc without Apple installRosetta"

log "simulate not_installed: start with rosetta=true must refuse"
cat >"$out/refuse.toml" <<EOF
name = "vz-rosetta-refuse"
backend = "vz"
rosetta = true
cpus = 1
ram_mib = 256
kernel = "$assets/Image"
initrd = "$assets/initramfs.cpio"
serial_log = "$out/refuse-serial.log"
cmdline = "console=hvc0 rdinit=/init"
[tpm]
enabled = false
EOF
set +e
TERNVALE_ROSETTA_AVAILABILITY=not_installed \
  "$root/scripts/sign-and-run.sh" "$bin" run "$out/refuse.toml" \
  >"$out/refuse.out" 2>"$out/refuse.err"
refuse_rc=$?
set -e
cat "$out/refuse.err" "$out/refuse.out" >"$out/refuse.log"
[[ "$refuse_rc" -ne 0 ]] || fail "not_installed start succeeded"
grep -qi 'not installed\|install-rosetta\|never installs' "$out/refuse.log" \
  || fail "refuse error was unclear: $(tail -20 "$out/refuse.log")"
log "not_installed start refused (rc=$refuse_rc)"

# --- x86_64 static binaries (host docker, linux/amd64) ---
log "compile x86_64 musl probes"
[[ -f "$assets/Image" && -f "$assets/initramfs.cpio" ]] || fail "missing $assets"
if ! docker info >/dev/null 2>&1; then
  fail "docker is not reachable; need it to build linux/amd64 static probes"
fi
mkdir -p "$out/x86"
cp "$root/guest-tests/rosetta-hello.c" "$root/guest-tests/rosetta-mt.c" "$out/x86/"
docker run --rm --platform linux/amd64 -v "$out/x86:/build" alpine:3.20 sh -c '
set -e
apk add --no-cache gcc musl-dev >/dev/null
gcc -static -Os -Wall -Werror -o /build/hello /build/rosetta-hello.c
gcc -static -Os -Wall -Werror -pthread -o /build/mt /build/rosetta-mt.c
'
file "$out/x86/hello" "$out/x86/mt" | tee "$out/x86-file.txt"
python3 - "$out/x86/hello" "$out/x86/mt" <<'PY' || fail "probes are not x86_64 ELF"
import pathlib, sys
for p in sys.argv[1:]:
    b = pathlib.Path(p).read_bytes()
    if b[0:4] != b"\x7fELF":
        sys.exit(f"{p} not ELF")
    if b[4] != 2 or b[18:20] != b"\x3e\x00":
        sys.exit(f"{p} is not ELFCLASS64 EM_X86_64")
    print(p, "ELF64 x86_64 ok", len(b))
PY

# --- binfmt_misc.ko matching virtio-devices linux-virt ---
cache="$root/target/vz-rosetta-cache"
mkdir -p "$cache"
if [[ ! -f "$cache/binfmt_misc.ko" ]]; then
  log "fetch binfmt_misc.ko from Alpine linux-virt"
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
    sys.exit("linux-virt not in APKINDEX")
PY
  )
  curl -fsSL --retry 3 -A Ternvale -o "$cache/linux-virt.apk" \
    "https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}/main/aarch64/${virt_apk}"
  tar -tf "$cache/linux-virt.apk" >"$cache/files"
  rel=$(grep -E 'binfmt_misc\.ko' "$cache/files" | head -1) || fail "no binfmt_misc.ko in linux-virt"
  case "$rel" in
    *.ko.gz) tar -xOf "$cache/linux-virt.apk" "$rel" | gzip -dc >"$cache/binfmt_misc.ko" ;;
    *.ko) tar -xOf "$cache/linux-virt.apk" "$rel" >"$cache/binfmt_misc.ko" ;;
    *) fail "unsupported $rel" ;;
  esac
  log "cached $cache/binfmt_misc.ko from $rel"
fi

# --- initramfs with live script ---
log "pack initramfs"
mkdir -p "$out/initrd"
(cd "$out/initrd" && cpio -id --quiet <"$assets/initramfs.cpio")
mkdir -p "$out/initrd/x86"
cp "$out/x86/hello" "$out/initrd/x86/hello"
cp "$out/x86/mt" "$out/initrd/x86/mt"
chmod 755 "$out/initrd/x86/hello" "$out/initrd/x86/mt"
cp "$cache/binfmt_misc.ko" "$out/initrd/modules/binfmt_misc.ko"
echo binfmt_misc >>"$out/initrd/modules/order"

# Apple "Running Intel Binaries in Linux VMs" magic/mask; flags P+C+F
# (preserve, credentials, fix-binary) match --preserve/--credentials/--fix-binary yes.
cat >"$out/initrd/rosettalive.sh" <<'GUEST'
#!/bin/busybox sh
echo ROSETTA_LIVE_START
/bin/busybox mkdir -p /mnt/rosetta /proc/sys/fs/binfmt_misc
if /bin/busybox mount -t virtiofs rosetta /mnt/rosetta; then
  echo ROSETTA_MOUNT_OK
  /bin/busybox ls -ld /mnt/rosetta /mnt/rosetta/* 2>/dev/null || true
else
  echo ROSETTA_MOUNT_FAIL
fi
RUNTIME=/mnt/rosetta/rosetta
if [ -d "$RUNTIME" ] && [ -e "$RUNTIME/rosetta" ]; then
  RUNTIME="$RUNTIME/rosetta"
fi
if [ -f "$RUNTIME" ]; then
  echo ROSETTA_RUNTIME_OK path=$RUNTIME
else
  echo ROSETTA_RUNTIME_MISSING
  RUNTIME=""
fi
if /bin/busybox mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc 2>/dev/null \
  || /bin/busybox mount -t binfmt_misc none /proc/sys/fs/binfmt_misc 2>/dev/null; then
  echo BINFMT_MOUNT_OK
else
  echo BINFMT_MOUNT_FAIL
fi
if [ -n "$RUNTIME" ]; then
  # Direct invoke before binfmt so a bad register cannot steal /bin/busybox.
  "$RUNTIME" /x86/hello
  echo DIRECT_HELLO_RC=$?
  "$RUNTIME" /x86/mt
  echo DIRECT_MT_RC=$?
  # Apple flags: credentials + preserve + fix-binary → C, P, F
  # Kernel binfmt-misc register parses \xHH itself; do not decode first (%b).
  /bin/busybox printf '%s' ":rosetta:M::\\x7fELF\\x02\\x01\\x01\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x02\\x00\\x3e\\x00:\\xff\\xff\\xff\\xff\\xff\\xfe\\xfe\\x00\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xfe\\xff\\xff\\xff:${RUNTIME}:PCF" \
    >/proc/sys/fs/binfmt_misc/register && echo BINFMT_REG_OK || echo BINFMT_REG_FAIL
fi
if [ -f /proc/sys/fs/binfmt_misc/rosetta ]; then
  echo BINFMT_ENTRY_OK
else
  echo BINFMT_ENTRY_MISSING
fi
/x86/hello
echo HELLO_RC=$?
/x86/mt
echo MT_RC=$?
if command -v docker >/dev/null 2>&1; then
  docker run --rm --platform linux/amd64 alpine uname -m
  echo DOCKER_RC=$?
else
  echo DOCKER_SKIP
fi
echo ROSETTA_LIVE_DONE
GUEST
chmod 755 "$out/initrd/rosettalive.sh"

python3 - "$out/initrd/init" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
text = p.read_text()
needle = "exec setsid sh -c 'exec sh -i'"
hook = """if /bin/busybox grep -q 'ternvale.rosettalive=1' /proc/cmdline; then
  /bin/busybox sh /rosettalive.sh
fi
exec setsid sh -c 'exec sh -i'"""
if needle not in text:
    sys.exit("init has no interactive shell exec to hook")
p.write_text(text.replace(needle, hook, 1))
PY
(cd "$out/initrd" && find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc) \
  >"$out/initramfs.cpio" 2>/dev/null

wait_serial() {
  local file=$1 pattern=$2 timeout=${3:-90}
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

run_one() {
  local name=$1 toml=$2 tag=$3
  "$root/scripts/sign-and-run.sh" "$bin" run "$toml" \
    </dev/null >"$out/${tag}.stdout" 2>"$out/${tag}.stderr" &
  local pid=$!
  if ! wait_serial "$out/${tag}-serial.log" "ROSETTA_LIVE_DONE" 120; then
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$name" --force >/dev/null 2>&1 || true
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    fail "$tag: no ROSETTA_LIVE_DONE (see $out/${tag}-serial.log $out/${tag}.stderr)"
  fi
  TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$name" --force >/dev/null 2>&1 || true
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  cp "$out/${tag}-serial.log" "$out/${tag}-serial.copy.log"
}

write_toml() {
  local name=$1 rosetta=$2 serial=$3
  cat >"$out/${name}.toml" <<EOF
name = "$name"
backend = "vz"
rosetta = $rosetta
cpus = 2
ram_mib = 512
kernel = "$assets/Image"
initrd = "$out/initramfs.cpio"
serial_log = "$serial"
cmdline = "console=hvc0 rdinit=/init ternvale.rosettalive=1"
[tpm]
enabled = false
EOF
}

log "guest rosetta=true"
write_toml vz-rosetta-on true "$out/on-serial.log"
run_one vz-rosetta-on "$out/vz-rosetta-on.toml" on
python3 - "$out/on-serial.log" <<'PY' || fail "rosetta=true guest checks"
from pathlib import Path
import sys
text = Path(sys.argv[1]).read_text(errors="replace")
need = [
    "ROSETTA_MOUNT_OK",
    "ROSETTA_RUNTIME_OK",
    "BINFMT_ENTRY_OK",
    "x86_hello",
    "DIRECT_HELLO_RC=0",
    "DIRECT_MT_RC=0",
    "mt_x86 n=4",
    "HELLO_RC=0",
    "MT_RC=0",
]
for n in need:
    if n not in text:
        sys.exit(f"missing {n}")
print("rosetta=true: mount, binfmt, static x86, mt x86 ok")
if "DOCKER_SKIP" in text:
    print("docker: skipped (no docker in initramfs guest)")
elif "x86_64" in text and "DOCKER" in text:
    print("docker: guest printed x86_64")
else:
    print("docker: not confirmed")
PY

log "guest rosetta=false"
write_toml vz-rosetta-off false "$out/off-serial.log"
run_one vz-rosetta-off "$out/vz-rosetta-off.toml" off
python3 - "$out/off-serial.log" <<'PY' || fail "rosetta=false must not set up translation"
from pathlib import Path
import sys
text = Path(sys.argv[1]).read_text(errors="replace")
if "ROSETTA_MOUNT_OK" in text:
    sys.exit("share mounted with rosetta=false")
if "BINFMT_ENTRY_OK" in text:
    sys.exit("binfmt registered with rosetta=false")
if "HELLO_RC=0" in text or "MT_RC=0" in text:
    sys.exit("x86 probes succeeded with rosetta=false")
if "ROSETTA_MOUNT_FAIL" not in text and "ROSETTA_LIVE_DONE" not in text:
    sys.exit("guest did not finish")
print("rosetta=false: no mount, no binfmt, x86 probes did not succeed")
PY

log "PASS"
log "artifacts $out"
rm -rf "$run_dir"
