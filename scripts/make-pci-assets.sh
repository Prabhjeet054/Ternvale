#!/bin/bash
# Add what the `pci` boot scenario needs on top of ./scripts/make-rootfs.sh output:
#   - pciutils (`lspci -nn`) and its pci.ids database installed into rootfs.ext4,
#     because Alpine's busybox has no lspci applet;
#   - data.ext4, a small ext4 disk holding /hello, attached as a second virtio-pci
#     disk (00:02.0, /dev/vdb) that the guest mounts by hand.
#
# Like make-rootfs.sh this needs Docker (linux/arm64, privileged for the loop mount)
# and network access from the container to the Alpine package mirror.
#
# Usage:
#   ./scripts/make-pci-assets.sh [test-assets/virtio-root]
#
# Safe to rerun: pciutils is only installed when /usr/bin/lspci is missing, and
# data.ext4 is rebuilt each time.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/virtio-root}"
out=$(cd "${out}" && pwd)
alpine_ver="${ALPINE_VER:-3.20}"
data_mib="${DATA_MIB:-32}"

log() {
    printf 'make-pci-assets: %s\n' "$*" >&2
}

if [[ ! -f "${out}/rootfs.ext4" ]]; then
    log "missing ${out}/rootfs.ext4 (run ./scripts/make-rootfs.sh first)"
    exit 1
fi
if ! docker info >/dev/null 2>&1; then
    log "docker is not reachable; start Docker Desktop and retry"
    exit 1
fi

log "installing pciutils into ${out}/rootfs.ext4 and building data.ext4 (${data_mib} MiB)"
docker run --rm --privileged --platform linux/arm64 \
    -v "${out}:/out" \
    -e "DATA_MIB=${data_mib}" \
    "alpine:${alpine_ver}" \
    sh -c '
set -euo pipefail
apk add --no-cache e2fsprogs >/dev/null
mkdir -p /mnt
mount -o loop /out/rootfs.ext4 /mnt
if [ -x /mnt/usr/bin/lspci ]; then
  echo "pciutils already installed"
else
  apk add --root /mnt --no-cache \
    --repositories-file /mnt/etc/apk/repositories \
    --keys-dir /mnt/etc/apk/keys \
    pciutils hwdata-pci
fi
/mnt/usr/bin/lspci --version 2>/dev/null || ls -l /mnt/usr/bin/lspci
ls -l /mnt/usr/share/hwdata/pci.ids
sync
umount /mnt
e2fsck -fy /out/rootfs.ext4 >/dev/null || [ $? -eq 1 ]

dd if=/dev/zero of=/out/data.ext4 bs=1M count="${DATA_MIB}" status=none
mkfs.ext4 -F -q -L ternvale-data /out/data.ext4
mount -o loop /out/data.ext4 /mnt
echo "ternvale pci data disk" > /mnt/hello
sync
umount /mnt
e2fsck -fn /out/data.ext4
'
log "done: $(ls -l "${out}/rootfs.ext4" "${out}/data.ext4" | awk '{print $5, $9}' | tr '\n' ' ')"
