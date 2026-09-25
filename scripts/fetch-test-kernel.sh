#!/bin/bash
# Download a minimal arm64 Linux Image and a busybox initramfs into test-assets/.
#
# The Image is Alpine 3.20's aarch64 virt vmlinuz. That file is an EFI zboot
# (gzip) wrapper. This script unpacks it to the raw Image whose header magic is
# ARM\x64. The initramfs is a newc cpio with Alpine's static busybox as /init.
#
# Buildroot alternative, if you would rather compile both:
#   git clone https://github.com/buildroot/buildroot.git
#   cd buildroot
#   make qemu_aarch64_virt_defconfig
#   make menuconfig   # set Target packages -> BusyBox, and Kernel
#   make
#   cp output/images/Image output/images/rootfs.cpio ../test-assets/
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="${root}/test-assets"
alpine="https://dl-cdn.alpinelinux.org/alpine/v3.20"
mkdir -p "${out}"

log() {
    printf 'fetch-test-kernel: %s\n' "$*" >&2
}

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

log "downloading Alpine virt vmlinuz"
curl -fL --retry 3 -A Ternvale -o "${tmp}/vmlinuz" \
    "${alpine}/releases/aarch64/netboot/vmlinuz-virt"

log "unpacking EFI zboot gzip to ${out}/Image"
python3 - "${tmp}/vmlinuz" "${out}/Image" <<'PY'
import gzip, pathlib, struct, sys
blob = pathlib.Path(sys.argv[1]).read_bytes()
if blob[4:8] != b"zimg":
    sys.exit("vmlinuz is not an EFI zboot image")
off, size = struct.unpack_from("<II", blob, 8)
comp = blob[24:32].split(b"\x00", 1)[0]
payload = blob[off:off + size]
if comp == b"gzip":
    image = gzip.decompress(payload)
else:
    sys.exit(f"unsupported zboot compression {comp!r}")
if image[56:60] != b"ARM\x64":
    sys.exit("unpacked file is not an arm64 Image")
pathlib.Path(sys.argv[2]).write_bytes(image)
text, image_size = struct.unpack_from("<QQ", image, 8)
print(f"text_offset={text:#x} image_size={image_size:#x} bytes={len(image)}", file=sys.stderr)
PY

log "downloading busybox"
curl -fL --retry 3 -A Ternvale -o "${tmp}/apkindex.tar.gz" \
    "${alpine}/main/aarch64/APKINDEX.tar.gz"
tar -xOf "${tmp}/apkindex.tar.gz" APKINDEX > "${tmp}/APKINDEX"
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
apk="$(apk_name busybox)"
musl="$(apk_name musl)"
curl -fL --retry 3 -A Ternvale -o "${tmp}/busybox.apk" "${alpine}/main/aarch64/${apk}"
curl -fL --retry 3 -A Ternvale -o "${tmp}/musl.apk" "${alpine}/main/aarch64/${musl}"
mkdir -p "${tmp}/root/bin" "${tmp}/root/lib"
tar -xOf "${tmp}/busybox.apk" bin/busybox > "${tmp}/root/bin/busybox"
tar -xOf "${tmp}/musl.apk" lib/ld-musl-aarch64.so.1 > "${tmp}/root/lib/ld-musl-aarch64.so.1"
chmod 755 "${tmp}/root/lib/ld-musl-aarch64.so.1"
ln -sf ld-musl-aarch64.so.1 "${tmp}/root/lib/libc.musl-aarch64.so.1"
chmod 755 "${tmp}/root/bin/busybox"
cp "${tmp}/root/bin/busybox" "${tmp}/root/bin/sh"
printf '%s\n' '#!/bin/busybox sh' 'exec /bin/busybox sh -i' > "${tmp}/root/init"
chmod 755 "${tmp}/root/init"

log "packing ${out}/initramfs.cpio"
(
    cd "${tmp}/root"
    # Linux looks up /init. A leading ./ makes that lookup miss.
    find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc
) > "${out}/initramfs.cpio"

log "wrote ${out}/Image and ${out}/initramfs.cpio"
