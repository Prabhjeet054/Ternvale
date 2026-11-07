#!/bin/bash
# Build test-assets/virtio-devices/{Image,initramfs.cpio} for the virtio-rng +
# virtio-vsock boot scenario.
#
# Like make-net-initramfs.sh, the kernel Image and modules come from one Alpine
# linux-virt package, and the load order of virtio_mmio, virtio_rng, and
# vmw_vsock_virtio_transport comes from its modules.dep. guest-tests/vsock-ping.c
# is compiled static for aarch64 musl in a linux/arm64 Alpine container (busybox
# has no vsock support). The guest agent is built static by
# build-guest-agent.sh. The initramfs /init loads the modules, waits for
# /dev/hwrng and /dev/vsock, prints "ternvale devices ready rng=<current>",
# starts /bin/ternvale-agent in the background when the kernel cmdline has
# ternvale.agent=<1|trace|debug|info|warn>, and starts a shell.
#
# Usage:
#   ./scripts/make-devices-initramfs.sh [OUT_DIR]
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/virtio-devices}"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() {
    printf 'make-devices-initramfs: %s\n' "$*" >&2
}

for tool in curl python3 gzip cpio tar docker; do
    command -v "${tool}" >/dev/null 2>&1 || {
        log "missing required tool: ${tool}"
        exit 1
    }
done
if ! docker info >/dev/null 2>&1; then
    log "docker is not reachable; start Docker Desktop and retry"
    exit 1
fi

mkdir -p "${out}"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

log "compiling guest-tests/vsock-ping.c and virtiofs-sec.c (static aarch64 musl)"
mkdir -p "${tmp}/build"
cp "${root}/guest-tests/vsock-ping.c" "${root}/guest-tests/virtiofs-sec.c" "${tmp}/build/"
docker run --rm --platform linux/arm64 -v "${tmp}/build:/build" "alpine:${alpine_ver}" sh -c '
set -e
apk add --no-cache gcc musl-dev linux-headers >/dev/null
gcc -static -Os -Wall -Werror -o /build/vsock-ping /build/vsock-ping.c
gcc -static -Os -Wall -Werror -o /build/virtiofs-sec /build/virtiofs-sec.c
'
"${script_dir}/build-guest-agent.sh" "${tmp}/build/ternvale-agent"

curl -fsSL --retry 3 -A Ternvale -o "${tmp}/apkindex.tar.gz" \
    "${alpine}/main/aarch64/APKINDEX.tar.gz"
tar -xOf "${tmp}/apkindex.tar.gz" APKINDEX >"${tmp}/APKINDEX"
apk_name() {
    python3 - "${tmp}/APKINDEX" "$1" <<'PY'
import sys
text = open(sys.argv[1], encoding="utf-8", errors="replace").read().split("\n\n")
for block in text:
    fields = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
    if fields.get("P") == sys.argv[2]:
        print(fields["P"] + "-" + fields["V"] + ".apk")
        break
else:
    sys.exit(f"{sys.argv[2]} package not found")
PY
}
for pkg in linux-virt busybox musl; do
    name="$(apk_name "${pkg}")"
    log "downloading ${name}"
    curl -fsSL --retry 3 -A Ternvale -o "${tmp}/${pkg}.apk" "${alpine}/main/aarch64/${name}"
done

log "unpacking boot/vmlinuz-virt to ${out}/Image"
tar -xOf "${tmp}/linux-virt.apk" boot/vmlinuz-virt >"${tmp}/vmlinuz"
python3 - "${tmp}/vmlinuz" "${out}/Image" <<'PY'
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
PY

tar -tf "${tmp}/linux-virt.apk" >"${tmp}/files"
mod_dir=$(grep -m1 '/modules.dep$' "${tmp}/files" | sed 's|/modules.dep$||')
[[ -n "${mod_dir}" ]] || { log "modules.dep not found in linux-virt"; exit 1; }
tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/modules.dep" >"${tmp}/modules.dep"
python3 - "${tmp}/modules.dep" virtio_mmio virtio_rng vmw_vsock_virtio_transport virtiofs fuse \
    9p 9pnet 9pnet_virtio \
    >"${tmp}/order" <<'PY'
import sys
deps = {}
for line in open(sys.argv[1]):
    if ":" in line:
        mod, rest = line.split(":", 1)
        deps[mod.strip()] = rest.split()
by_name = {p.rsplit("/", 1)[1].split(".ko")[0].replace("-", "_"): p for p in deps}
order = []
def visit(path):
    for dep in deps.get(path, []):
        visit(dep)
    if path not in order:
        order.append(path)
for want in sys.argv[2:]:
    if want not in by_name:
        print(f"{want} is not a module in this kernel (built in?); skip", file=sys.stderr)
        continue
    visit(by_name[want])
print("\n".join(order))
PY

mkdir -p "${tmp}/initrd/bin" "${tmp}/initrd/lib" "${tmp}/initrd/modules"
tar -xOf "${tmp}/busybox.apk" bin/busybox >"${tmp}/initrd/bin/busybox"
tar -xOf "${tmp}/musl.apk" lib/ld-musl-aarch64.so.1 >"${tmp}/initrd/lib/ld-musl-aarch64.so.1"
cp "${tmp}/build/vsock-ping" "${tmp}/initrd/bin/vsock-ping"
cp "${tmp}/build/virtiofs-sec" "${tmp}/initrd/bin/virtiofs-sec"
cp "${tmp}/build/ternvale-agent" "${tmp}/initrd/bin/ternvale-agent"
chmod 755 "${tmp}/initrd/bin/busybox" "${tmp}/initrd/bin/vsock-ping" \
    "${tmp}/initrd/bin/virtiofs-sec" "${tmp}/initrd/bin/ternvale-agent" \
    "${tmp}/initrd/lib/ld-musl-aarch64.so.1"
ln -sf ld-musl-aarch64.so.1 "${tmp}/initrd/lib/libc.musl-aarch64.so.1"

: >"${tmp}/initrd/modules/order"
while read -r rel; do
    name=$(basename "${rel}")
    base="${name%%.ko*}"
    case "${name}" in
        *.ko.gz) tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/${rel}" | gzip -dc >"${tmp}/initrd/modules/${base}.ko" ;;
        *.ko) tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/${rel}" >"${tmp}/initrd/modules/${base}.ko" ;;
        *) log "unsupported module compression: ${name}"; exit 1 ;;
    esac
    printf '%s\n' "${base}" >>"${tmp}/initrd/modules/order"
    log "module ${base}"
done <"${tmp}/order"

cat >"${tmp}/initrd/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /sbin /usr/bin /usr/sbin
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
/bin/busybox --install -s
for m in $(cat /modules/order); do
  insmod /modules/${m}.ko || echo "insmod ${m} failed"
done
i=0
while { [ ! -e /dev/hwrng ] || [ ! -e /dev/vsock ]; } && [ "$i" -lt 100 ]; do
  sleep 0.1
  i=$((i + 1))
done
rng=$(cat /sys/class/misc/hw_random/rng_current 2>/dev/null)
echo "ternvale devices ready rng=${rng:-missing} vsock=$([ -e /dev/vsock ] && echo yes || echo no) virtiofs=$([ -d /sys/module/virtiofs ] && echo yes || echo no)"
agent=$(sed -n 's/.*ternvale\.agent=\([a-z0-9]*\).*/\1/p' /proc/cmdline)
sharetest=$(sed -n 's/.*ternvale\.sharetest=1.*/1/p' /proc/cmdline)
fstest=$(sed -n 's/.*ternvale\.fstest=1.*/1/p' /proc/cmdline)
rotest=$(sed -n 's/.*ternvale\.rotest=1.*/1/p' /proc/cmdline)
if [ -n "${agent}" ]; then
  case "${agent}" in trace|debug|info|warn) level=${agent} ;; *) level=info ;; esac
  if [ "${sharetest}" = "1" ] || [ "${fstest}" = "1" ] || [ "${rotest}" = "1" ]; then
    export TERNVALE_AGENT_HARNESS=1
  fi
  TERNVALE_AGENT_LOG=${level} setsid /bin/ternvale-agent </dev/null >/dev/console 2>&1 &
  echo "ternvale agent started pid=$! log=${level}"
fi
if [ "${sharetest}" = "1" ]; then
  /bin/busybox sh /sharetest.sh
  exit 0
fi
if [ "${fstest}" = "1" ]; then
  /bin/busybox sh /fstest.sh
  exit 0
fi
if [ "${rotest}" = "1" ]; then
  /bin/busybox sh /rotest.sh
  exit 0
fi
exec setsid sh -c 'exec sh -i' </dev/console >/dev/console 2>&1
INIT
chmod 755 "${tmp}/initrd/init"

cat >"${tmp}/initrd/sharetest.sh" <<'SHARE'
#!/bin/busybox sh
set -e
MP=/mnt/ternvale
HOST_CLIP="host-copy-paste-test"
GUEST_CLIP="guest-copy-paste-test"
i=0
while ! /bin/busybox mountpoint -q "${MP}" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 300 ]; then
    echo "SHARE_MOUNT_TIMEOUT"
    exit 1
  fi
  /bin/busybox sleep 0.1
done
echo "SHARE_MOUNTED mount=${MP}"
i=0
while [ ! -f /tmp/ternvale-host-clipboard-out ]; do
  i=$((i + 1))
  if [ "$i" -gt 300 ]; then
    echo "CLIP_HOST_TIMEOUT"
    exit 1
  fi
  /bin/busybox sleep 0.1
done
got=$(/bin/busybox cat /tmp/ternvale-host-clipboard-out)
if [ "${got}" != "${HOST_CLIP}" ]; then
  echo "CLIP_HOST_BAD got=${got}"
  exit 1
fi
echo "CLIP_HOST_OK len=${#got}"
echo -n "${GUEST_CLIP}" >/tmp/ternvale-guest-clipboard
echo "CLIP_GUEST_READY"
if [ ! -f "${MP}/from_host.bin" ]; then
  echo "HOST_FILE_MISSING"
  exit 1
fi
host_sha=$(/bin/busybox sha256sum "${MP}/from_host.bin" | /bin/busybox awk '{print $1}')
echo "HOST_FILE_SHA=${host_sha}"
/bin/busybox dd if=/dev/urandom of="${MP}/from_guest.bin" bs=1M count=5 status=none
guest_sha=$(/bin/busybox sha256sum "${MP}/from_guest.bin" | /bin/busybox awk '{print $1}')
echo "GUEST_FILE_SHA=${guest_sha}"
echo "SHARE_CLIPBOARD_TEST_DONE"
/bin/busybox poweroff -f
SHARE
chmod 755 "${tmp}/initrd/sharetest.sh"

cat >"${tmp}/initrd/fstest.sh" <<'FSTEST'
#!/bin/busybox sh
MP=/mnt/ternvale
i=0
while ! /bin/busybox mountpoint -q "${MP}" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 600 ]; then
    echo "SHARE_MOUNT_TIMEOUT"
    exit 1
  fi
  /bin/busybox sleep 0.1
done
echo "SHARE_MOUNTED mount=${MP} type=$(/bin/busybox awk '$2=="/mnt/ternvale"{print $3}' /proc/mounts)"

/bin/busybox mkdir "${MP}/posix" || { echo "POSIX_MKDIR_FAIL"; exit 1; }
echo hello >"${MP}/posix/a.txt" || { echo "POSIX_WRITE_FAIL"; exit 1; }
got=$(/bin/busybox cat "${MP}/posix/a.txt")
if [ "${got}" != "hello" ]; then
  echo "POSIX_READ_FAIL got=${got}"
  exit 1
fi
/bin/busybox mv "${MP}/posix/a.txt" "${MP}/posix/b.txt" || { echo "POSIX_RENAME_FAIL"; exit 1; }
/bin/busybox ln -s b.txt "${MP}/posix/rel" || { echo "POSIX_SYMLINK_FAIL"; exit 1; }
list=$(/bin/busybox ls "${MP}/posix")
echo "POSIX_READDIR=${list}"
/bin/busybox rm "${MP}/posix/rel" "${MP}/posix/b.txt"
/bin/busybox rmdir "${MP}/posix"
echo "POSIX_SUBSET_OK"

/bin/virtiofs-sec "${MP}"
echo "SEC_PROBE_DONE"

if [ ! -f "${MP}/from_host.bin" ]; then
  echo "HOST_FILE_MISSING"
  exit 1
fi
t0=$(/bin/busybox awk '{print $1}' /proc/uptime)
host_sha=$(/bin/busybox sha256sum "${MP}/from_host.bin" | /bin/busybox awk '{print $1}')
t1=$(/bin/busybox awk '{print $1}' /proc/uptime)
echo "HOST_FILE_SHA=${host_sha}"
echo "COPY_H2G_UPTIME_S=${t0}->${t1}"
t2=$(/bin/busybox awk '{print $1}' /proc/uptime)
/bin/busybox dd if="${MP}/from_host.bin" of="${MP}/from_guest.bin" bs=1M status=none || {
  echo "GUEST_COPY_FAIL"
  exit 1
}
t3=$(/bin/busybox awk '{print $1}' /proc/uptime)
guest_sha=$(/bin/busybox sha256sum "${MP}/from_guest.bin" | /bin/busybox awk '{print $1}')
echo "GUEST_FILE_SHA=${guest_sha}"
echo "COPY_G2H_UPTIME_S=${t2}->${t3}"
echo "SHARE_FSTEST_DONE"
/bin/busybox poweroff -f
FSTEST
chmod 755 "${tmp}/initrd/fstest.sh"

cat >"${tmp}/initrd/rotest.sh" <<'ROTEST'
#!/bin/busybox sh
MP=/mnt/ternvale
i=0
while ! /bin/busybox mountpoint -q "${MP}" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 600 ]; then
    echo "SHARE_MOUNT_TIMEOUT"
    exit 1
  fi
  /bin/busybox sleep 0.1
done
echo "SHARE_MOUNTED mount=${MP}"
set +e
echo ro-write >"${MP}/should_fail.txt" 2>/tmp/ro.err
echo "WRITE_RC=$?"
/bin/busybox cat /tmp/ro.err
echo "SHARE_ROTEST_DONE"
/bin/busybox poweroff -f
ROTEST
chmod 755 "${tmp}/initrd/rotest.sh"

(
    cd "${tmp}/initrd"
    find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc
) >"${out}/initramfs.cpio"

log "kernel modules dir ${mod_dir}"
log "wrote ${out}/Image and ${out}/initramfs.cpio"
