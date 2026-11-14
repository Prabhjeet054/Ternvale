#!/usr/bin/env bash
# Step 97 live: ternvale-guest-setup on Debian, Ubuntu, Fedora, Alpine VZ guests.
# Persistent root disk + reboot. PID 1 is guest-tools/lib/guest-setup/live-init.sh
# (docker-exported userspace; systemd unit/fstab/local.d applied on second boot).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
stamp="$(date +%Y%m%d-%H%M%S)"
out="$root/target/vz-guest-setup/${stamp}"
run_dir="/tmp/tvgs-$$"
bin="$target/debug/ternvale"
assets="$root/test-assets/virtio-root"
cache="$root/target/vz-guest-setup-cache"
alpine_ver="${ALPINE_VER:-3.20}"
log() { printf 'vz-guest-setup-check: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

mkdir -p "$out" "$run_dir" "$cache"
export TERNVALE_RUN_DIR="$run_dir"
export TERNVALE_SKIP_SANDBOX=1
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::rosetta=debug,ternvale::vz=debug,ternvale::cli=info}"

cleanup() {
  for n in tvgs-debian tvgs-ubuntu tvgs-fedora tvgs-alpine; do
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$n" --force \
      >/dev/null 2>&1 || true
  done
  rm -rf "$run_dir"
}
trap cleanup EXIT

[[ -f "$assets/Image" && -f "$assets/initramfs.cpio" ]] || fail "missing $assets (make-rootfs.sh)"
command -v docker >/dev/null || fail "docker required"
docker info >/dev/null 2>&1 || fail "docker is not reachable"
command -v curl >/dev/null
command -v python3 >/dev/null

(cd "$root" && cargo build -p ternvale-cli -q)
"$root/scripts/sign-and-run.sh" "$bin" --help >/dev/null

log "x86_64 probes"
mkdir -p "$out/x86"
cp "$root/guest-tests/rosetta-hello.c" "$root/guest-tests/rosetta-mt.c" "$out/x86/"
docker run --rm --platform linux/amd64 -v "$out/x86:/build" alpine:3.20 sh -c '
set -e
apk add --no-cache gcc musl-dev >/dev/null
gcc -static -Os -Wall -Werror -o /build/hello /build/rosetta-hello.c
gcc -static -Os -Wall -Werror -pthread -o /build/mt /build/rosetta-mt.c
'

log "linux-virt Image + modules (same apk, so vermagic matches)"
if [[ ! -f "$cache/Image" || ! -f "$cache/modules/virtiofs.ko" ]]; then
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
  log "fetch $virt_apk"
  curl -fsSL --retry 3 -A Ternvale -o "$cache/linux-virt.apk" \
    "https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}/main/aarch64/${virt_apk}"
  tar -tf "$cache/linux-virt.apk" >"$cache/files"
  tar -xOf "$cache/linux-virt.apk" boot/vmlinuz-virt >"$cache/vmlinuz"
  python3 - "$cache/vmlinuz" "$cache/Image" <<'PY'
import gzip, pathlib, struct, sys
blob = pathlib.Path(sys.argv[1]).read_bytes()
if blob[56:60] == b"ARM\x64":
    image = blob
elif blob[4:8] == b"zimg":
    off, size = struct.unpack_from("<II", blob, 8)
    if blob[24:32].split(b"\x00", 1)[0] != b"gzip":
        sys.exit("unsupported zboot compression")
    image = gzip.decompress(blob[off:off + size])
else:
    sys.exit("vmlinuz-virt is neither a raw arm64 Image nor an EFI zboot image")
if image[56:60] != b"ARM\x64":
    sys.exit("unpacked file is not an arm64 Image")
pathlib.Path(sys.argv[2]).write_bytes(image)
print("Image bytes", len(image))
PY
fi
extract_ko() {
  local pat=$1 dest=$2
  local rel
  rel=$(grep -E "$pat" "$cache/files" | head -1) || return 1
  case "$rel" in
    *.ko.gz) tar -xOf "$cache/linux-virt.apk" "$rel" | gzip -dc >"$dest" ;;
    *.ko) tar -xOf "$cache/linux-virt.apk" "$rel" >"$dest" ;;
    *) return 1 ;;
  esac
  log "extracted $rel -> $dest"
}
mkdir -p "$cache/modules"
if [[ ! -f "$cache/modules/virtiofs.ko" ]]; then
  extract_ko 'virtio_mmio\.ko' "$cache/modules/virtio_mmio.ko" || true
  extract_ko 'virtio_blk\.ko' "$cache/modules/virtio_blk.ko"
  extract_ko 'crc16\.ko' "$cache/modules/crc16.ko"
  extract_ko 'crc32c_generic\.ko' "$cache/modules/crc32c_generic.ko"
  extract_ko 'libcrc32c\.ko' "$cache/modules/libcrc32c.ko"
  extract_ko 'mbcache\.ko' "$cache/modules/mbcache.ko"
  extract_ko 'jbd2\.ko' "$cache/modules/jbd2.ko"
  extract_ko 'ext4\.ko' "$cache/modules/ext4.ko"
  extract_ko 'fuse\.ko' "$cache/modules/fuse.ko"
  extract_ko 'virtiofs\.ko' "$cache/modules/virtiofs.ko"
  extract_ko 'binfmt_misc\.ko' "$cache/modules/binfmt_misc.ko"
fi

log "initramfs with switch_root + virtiofs"
mkdir -p "$out/initrd"
(cd "$out/initrd" && cpio -id --quiet <"$assets/initramfs.cpio")
mkdir -p "$out/initrd/modules"
cp "$cache/modules/"*.ko "$out/initrd/modules/"
python3 - "$out/initrd/init" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
text = p.read_text()
old = "for m in crc16 crc32c_generic libcrc32c mbcache jbd2 ext4 virtio_mmio virtio_blk; do"
new = "for m in crc16 crc32c_generic libcrc32c mbcache jbd2 ext4 virtio_mmio virtio_blk fuse virtiofs binfmt_misc; do"
if old not in text:
    sys.exit("initramfs /init module list not found")
p.write_text(text.replace(old, new, 1))
PY
(cd "$out/initrd" && find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc) \
  >"$out/initramfs.cpio" 2>/dev/null

build_disk() {
  local distro=$1 image=$2 size=$3 systemd=$4
  local disk="$out/${distro}.ext4"
  log "rootfs $distro from $image (${size} MiB)"
  local cid
  docker rm -f "tvgs-src-$distro" >/dev/null 2>&1 || true
  cid=$(docker create --platform linux/arm64 --name "tvgs-src-$distro" "$image")
  docker export "$cid" >"$out/${distro}.tar"
  docker rm "$cid" >/dev/null
  docker run --rm --privileged --platform linux/arm64 \
    -v "$out:/work" \
    -v "$root/guest-tools:/tools:ro" \
    -v "$out/x86:/x86:ro" \
    -v "$cache/modules:/mod:ro" \
    -e DISTRO="$distro" -e SIZE="$size" -e SYSTEMD="$systemd" \
    alpine:3.20 sh -c '
set -euo pipefail
apk add --no-cache e2fsprogs >/dev/null
dd if=/dev/zero of=/work/${DISTRO}.ext4 bs=1M count="${SIZE}" status=none
mkfs.ext4 -F -L "tvgs-${DISTRO}" /work/${DISTRO}.ext4 >/dev/null
mkdir -p /mnt
mount -o loop /work/${DISTRO}.ext4 /mnt
tar -xf /work/${DISTRO}.tar -C /mnt
mkdir -p /mnt/usr/local/sbin /mnt/usr/local/lib/ternvale/guest-setup /mnt/x86 /mnt/lib/modules/tv /mnt/sbin /mnt/tmp
cp /tools/ternvale-guest-setup /mnt/usr/local/sbin/ternvale-guest-setup
cp /tools/lib/guest-setup/*.sh /mnt/usr/local/lib/ternvale/guest-setup/
# live-init is PID 1; keep a copy of distro init if present
if [ -e /mnt/sbin/init ] && [ ! -f /mnt/sbin/init.distro ]; then
  cp -a /mnt/sbin/init /mnt/sbin/init.distro || true
fi
cp /tools/lib/guest-setup/live-init.sh /mnt/sbin/init
chmod 0755 /mnt/sbin/init /mnt/usr/local/sbin/ternvale-guest-setup
cp /x86/hello /x86/mt /mnt/x86/
chmod 0755 /mnt/x86/hello /mnt/x86/mt
cp /mod/*.ko /mnt/lib/modules/tv/ 2>/dev/null || true
if [ "$SYSTEMD" = 1 ]; then
  mkdir -p /mnt/usr/lib/systemd/system /mnt/etc/systemd/system
fi
# debian/ubuntu/fedora need a working mount(8)
if [ -x /mnt/bin/busybox ]; then
  /mnt/bin/busybox --install -s /mnt/bin 2>/dev/null || true
fi
sync
umount /mnt
e2fsck -fy /work/${DISTRO}.ext4 >/dev/null
'
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

run_phase() {
  local distro=$1 phase=$2 timeout=$3
  local name="tvgs-${distro}"
  local toml="$out/${name}-${phase}.toml"
  local serial="$out/${distro}-${phase}-serial.log"
  cat >"$toml" <<EOF
name = "$name"
backend = "vz"
profile = "linux"
rosetta = true
cpus = 2
ram_mib = 1024
kernel = "$cache/Image"
initrd = "$out/initramfs.cpio"
boot_disk = true
cmdline = "console=hvc0 tvgs.phase=${phase}"
serial_log = "$serial"
[[disks]]
path = "$out/${distro}.ext4"
read_only = false
[tpm]
enabled = false
[audio]
enabled = false
[usb]
enabled = false
EOF
  : >"$serial"
  "$root/scripts/sign-and-run.sh" "$bin" run "$toml" \
    </dev/null >"$out/${distro}-${phase}.stdout" 2>"$out/${distro}-${phase}.stderr" &
  local pid=$!
  if ! wait_serial "$serial" "TVGS_LIVE_DONE" "$timeout"; then
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$name" --force >/dev/null 2>&1 || true
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    fail "$distro $phase: no TVGS_LIVE_DONE (serial $(wc -c <"$serial" 2>/dev/null || echo 0) bytes)"
  fi
  TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$name" --force >/dev/null 2>&1 || true
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

score_distro() {
  local distro=$1
  local first="$out/${distro}-first-serial.log"
  local reboot="$out/${distro}-reboot-serial.log"
  python3 - "$distro" "$first" "$reboot" "$out/${distro}.summary" <<'PY'
import pathlib, sys
distro, first_p, reboot_p, outp = sys.argv[1:]
first = pathlib.Path(first_p).read_text(errors="replace")
reboot = pathlib.Path(reboot_p).read_text(errors="replace")
rows = []
def check(name, ok):
    rows.append((name, "pass" if ok else "FAIL"))
    return ok
ok = True
ok &= check("dry-run no writes", "TVGS_DRY_OK" in first)
ok &= check("first apply", "TVGS_APPLY_DONE" in first)
ok &= check("idempotent second run", "TVGS_IDEMPOTENT_OK" in first)
ok &= check("x86 after apply", "HELLO_RC=0" in first and "x86_hello" in first)
ok &= check("binfmt after apply", "BINFMT_ENTRY_OK" in first)
ok &= check("x86 after reboot", "HELLO_RC=0" in reboot and "x86_hello" in reboot)
ok &= check("binfmt after reboot", "BINFMT_ENTRY_OK" in reboot)
ok &= check("undo restored files", "TVGS_UNDO_OK" in reboot)
# After undo, binfmt should not succeed via interpreterless exec (rc != 0). Direct
# /mnt/rosetta/rosetta may still work if the share is mounted.
ok &= check("undo serial marker", "TVGS_UNDO_DONE" in reboot)
text = "\n".join(f"| {k} | {v} |" for k, v in rows)
pathlib.Path(outp).write_text(text + "\n")
print(distro)
print(text)
if not ok:
    sys.exit(1)
PY
}

# Pull images then build disks.
for spec in \
  "debian debian:bookworm-slim 1024 1" \
  "ubuntu ubuntu:24.04 1024 1" \
  "fedora fedora:40 1536 1" \
  "alpine alpine:3.20 512 0"
do
  set -- $spec
  docker pull --platform linux/arm64 "$2"
  build_disk "$1" "$2" "$3" "$4"
done

summary_fail=0
for distro in debian ubuntu fedora alpine; do
  log "=== $distro first boot (dry-run + apply + second run) ==="
  run_phase "$distro" first 180
  log "=== $distro reboot + undo ==="
  run_phase "$distro" reboot 180
  if score_distro "$distro"; then
    log "$distro PASS"
  else
    log "$distro FAIL (see $out/${distro}.summary)"
    summary_fail=1
  fi
done

if [[ "$summary_fail" -ne 0 ]]; then
  fail "one or more distros failed; artifacts $out"
fi
log "PASS"
log "artifacts $out"
echo "$out" >"$root/target/vz-guest-setup/latest"
