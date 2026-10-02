#!/bin/bash
# Build an Alpine arm64 ext4 rootfs image and a switch_root initramfs for Ternvale.
#
# macOS has no mkfs.ext4. This script uses Docker (linux/arm64, privileged for loop
# mounts) once to produce:
#   test-assets/virtio-root/rootfs.ext4
#   test-assets/virtio-root/initramfs.cpio
#   test-assets/virtio-root/Image   (copied from virtio-blk or fetched)
#
# Prerequisites:
#   - Docker Desktop with linux/arm64 support
#   - curl, python3, gzip, cpio on the host
#   - Alpine linux-virt modules (downloaded) for virtio_blk/ext4
#
# Usage:
#   ./scripts/make-rootfs.sh
#   SIZE_MIB=512 ./scripts/make-rootfs.sh /path/to/out
#
# Kernel config notes: see docs/ROOTFS.md. Alpine's stock virt kernel ships
# CONFIG_VIRTIO_BLK=m and CONFIG_EXT4_FS=m; the initramfs loads those modules
# before switch_root. A custom kernel with =y does not need those modules.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/virtio-root}"
size_mib="${SIZE_MIB:-256}"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() {
    printf 'make-rootfs: %s\n' "$*" >&2
}

need() {
    command -v "$1" >/dev/null 2>&1 || {
        log "missing required tool: $1"
        exit 1
    }
}

need docker
need curl
need python3
need gzip
need cpio

if ! docker info >/dev/null 2>&1; then
    log "docker is not reachable; start Docker Desktop and retry"
    exit 1
fi

mkdir -p "${out}"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

log "downloading Alpine aarch64 minirootfs"
miniroot_url=$(curl -fsSL "${alpine}/releases/aarch64/" \
    | python3 -c "
import re,sys
html=sys.stdin.read()
m=re.findall(r'alpine-minirootfs-[0-9.]+-aarch64\\.tar\\.gz', html)
print(sorted(set(m))[-1] if m else '')
")
if [[ -z "${miniroot_url}" ]]; then
    log "could not find alpine-minirootfs on ${alpine}/releases/aarch64/"
    exit 1
fi
curl -fL --retry 3 -A Ternvale -o "${tmp}/miniroot.tar.gz" \
    "${alpine}/releases/aarch64/${miniroot_url}"

log "writing guest /sbin/init overlay"
mkdir -p "${tmp}/overlay/sbin" "${tmp}/overlay/root"
cat >"${tmp}/overlay/sbin/init" <<'INIT'
#!/bin/sh
/bin/busybox mkdir -p /proc /sys /dev /root
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev 2>/dev/null || true
/bin/busybox echo "ternvale root ready"
exec /bin/busybox setsid /bin/busybox sh -c 'exec /bin/busybox sh -i' \
    </dev/console >/dev/console 2>&1
INIT
chmod 755 "${tmp}/overlay/sbin/init"

log "building ${size_mib} MiB ext4 image via Docker (privileged loop mount)"
docker run --rm --privileged --platform linux/arm64 \
    -v "${tmp}/miniroot.tar.gz:/in/miniroot.tar.gz:ro" \
    -v "${tmp}/overlay:/overlay:ro" \
    -v "${out}:/out" \
    -e "SIZE_MIB=${size_mib}" \
    "alpine:${alpine_ver}" \
    sh -c '
set -euo pipefail
apk add --no-cache e2fsprogs >/dev/null
dd if=/dev/zero of=/out/rootfs.ext4 bs=1M count="${SIZE_MIB}" status=none
mkfs.ext4 -F -L ternvale-root /out/rootfs.ext4 >/dev/null
mkdir -p /mnt
mount -o loop /out/rootfs.ext4 /mnt
tar -xzf /in/miniroot.tar.gz -C /mnt
cp /overlay/sbin/init /mnt/sbin/init
chmod 755 /mnt/sbin/init
mkdir -p /mnt/root
sync
umount /mnt
e2fsck -fy /out/rootfs.ext4 >/dev/null
'

log "fetching linux-virt modules for switch_root initramfs"
curl -fL --retry 3 -A Ternvale -o "${tmp}/apkindex.tar.gz" \
    "${alpine}/main/aarch64/APKINDEX.tar.gz"
tar -xOf "${tmp}/apkindex.tar.gz" APKINDEX >"${tmp}/APKINDEX"
apk_name() {
    python3 - "${tmp}/APKINDEX" "$1" <<'PY'
import sys
text = open(sys.argv[1], encoding="utf-8", errors="replace").read().split("\n\n")
want = sys.argv[2]
for block in text:
    fields = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
    if fields.get("P") == want:
        print(fields["P"] + "-" + fields["V"] + ".apk")
        break
else:
    sys.exit(f"{want} package not found")
PY
}
virt_apk="$(apk_name linux-virt)"
busy_apk="$(apk_name busybox)"
musl_apk="$(apk_name musl)"
curl -fL --retry 3 -A Ternvale -o "${tmp}/linux-virt.apk" \
    "${alpine}/main/aarch64/${virt_apk}"
curl -fL --retry 3 -A Ternvale -o "${tmp}/busybox.apk" \
    "${alpine}/main/aarch64/${busy_apk}"
curl -fL --retry 3 -A Ternvale -o "${tmp}/musl.apk" \
    "${alpine}/main/aarch64/${musl_apk}"

log "assembling initramfs.cpio"
mkdir -p "${tmp}/initrd/bin" "${tmp}/initrd/lib" "${tmp}/initrd/modules"
tar -xOf "${tmp}/busybox.apk" bin/busybox >"${tmp}/initrd/bin/busybox"
tar -xOf "${tmp}/musl.apk" lib/ld-musl-aarch64.so.1 >"${tmp}/initrd/lib/ld-musl-aarch64.so.1"
chmod 755 "${tmp}/initrd/bin/busybox" "${tmp}/initrd/lib/ld-musl-aarch64.so.1"
ln -sf ld-musl-aarch64.so.1 "${tmp}/initrd/lib/libc.musl-aarch64.so.1"

extract_ko() {
    local member="$1"
    local dest="$2"
    if tar -tOf "${tmp}/linux-virt.apk" "${member}" >/dev/null 2>&1; then
        tar -xOf "${tmp}/linux-virt.apk" "${member}" | gzip -dc >"${dest}"
    else
        # Some packages ship uncompressed .ko
        tar -xOf "${tmp}/linux-virt.apk" "${member%.gz}" >"${dest}"
    fi
}

mod_prefix="lib/modules"
# Resolve the modules directory inside the apk.
mod_dir=$(tar -tf "${tmp}/linux-virt.apk" | python3 -c '
import sys
for line in sys.stdin:
    line=line.strip()
    if line.endswith("/kernel/drivers/block/virtio_blk.ko.gz") or line.endswith("/kernel/drivers/block/virtio_blk.ko"):
        print("/".join(line.split("/")[:3]))
        break
')
if [[ -z "${mod_dir}" ]]; then
    log "virtio_blk.ko not found in ${virt_apk}"
    exit 1
fi

extract_ko "${mod_dir}/kernel/drivers/virtio/virtio_mmio.ko.gz" "${tmp}/initrd/modules/virtio_mmio.ko"
extract_ko "${mod_dir}/kernel/drivers/block/virtio_blk.ko.gz" "${tmp}/initrd/modules/virtio_blk.ko"
extract_ko "${mod_dir}/kernel/lib/crc16.ko.gz" "${tmp}/initrd/modules/crc16.ko"
extract_ko "${mod_dir}/kernel/crypto/crc32c_generic.ko.gz" "${tmp}/initrd/modules/crc32c_generic.ko"
extract_ko "${mod_dir}/kernel/lib/libcrc32c.ko.gz" "${tmp}/initrd/modules/libcrc32c.ko"
extract_ko "${mod_dir}/kernel/fs/mbcache.ko.gz" "${tmp}/initrd/modules/mbcache.ko"
extract_ko "${mod_dir}/kernel/fs/jbd2/jbd2.ko.gz" "${tmp}/initrd/modules/jbd2.ko"
extract_ko "${mod_dir}/kernel/fs/ext4/ext4.ko.gz" "${tmp}/initrd/modules/ext4.ko"

cat >"${tmp}/initrd/init" <<'INITRD'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /newroot
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
for m in crc16 crc32c_generic libcrc32c mbcache jbd2 ext4 virtio_mmio virtio_blk; do
  /bin/busybox insmod /modules/${m}.ko || /bin/busybox echo "insmod ${m} failed"
done
i=0
while [ ! -e /sys/block/vda ] && [ "$i" -lt 100 ]; do
  /bin/busybox sleep 0.1
  i=$((i + 1))
done
if [ -e /sys/block/vda/dev ] && [ ! -b /dev/vda ]; then
  majmin=$(/bin/busybox cat /sys/block/vda/dev)
  major=${majmin%:*}
  minor=${majmin#*:}
  /bin/busybox mknod /dev/vda b "$major" "$minor"
fi
if [ ! -b /dev/vda ]; then
  /bin/busybox echo "vda missing"
  /bin/busybox ls -l /dev /sys/block 2>/dev/null
  exec /bin/busybox sh -i
fi
/bin/busybox echo "vda ready"
if ! /bin/busybox mount -t ext4 -o rw /dev/vda /newroot; then
  /bin/busybox echo "mount /dev/vda failed"
  /bin/busybox cat /proc/filesystems
  exec /bin/busybox sh -i
fi
/bin/busybox mkdir -p /newroot/proc /newroot/sys /newroot/dev
/bin/busybox mount --move /proc /newroot/proc || true
/bin/busybox mount --move /sys /newroot/sys || true
/bin/busybox mount --move /dev /newroot/dev || true
exec /bin/busybox switch_root /newroot /sbin/init
INITRD
chmod 755 "${tmp}/initrd/init"

(
    cd "${tmp}/initrd"
    find . | cpio -o -H newc
) >"${out}/initramfs.cpio"

if [[ -f "${root}/test-assets/virtio-blk/Image" ]]; then
    log "copying Image from test-assets/virtio-blk"
    cp -f "${root}/test-assets/virtio-blk/Image" "${out}/Image"
elif [[ -f "${root}/test-assets/Image" ]]; then
    log "copying Image from test-assets"
    cp -f "${root}/test-assets/Image" "${out}/Image"
else
    log "no Image found; run scripts/fetch-test-kernel.sh or copy Alpine virt Image to ${out}/Image"
fi

log "wrote ${out}/rootfs.ext4 ($(du -h "${out}/rootfs.ext4" | awk '{print $1}'))"
log "wrote ${out}/initramfs.cpio ($(du -h "${out}/initramfs.cpio" | awk '{print $1}'))"
[[ -f "${out}/Image" ]] && log "wrote ${out}/Image"
log "done"
