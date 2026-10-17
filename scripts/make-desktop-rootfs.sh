#!/bin/bash
# Build a minimal Linux desktop guest for Ternvale (docs/DESKTOP.md):
#   test-assets/desktop/rootfs.ext4      Alpine arm64: Xorg (modesetting + fbdev video,
#                                        evdev input), twm, xterm, xdotool, misc fonts
#   test-assets/desktop/Image            the rootfs's own linux-virt kernel
#   test-assets/desktop/initramfs.cpio   loads virtio_mmio/virtio_blk/ext4 from that
#                                        kernel, mounts /dev/vda, switch_root
#   test-assets/desktop/vm.toml.example  a `ternvale run --display window` config
#
# The guest-side files are in scripts/desktop-guest/: /sbin/init (modprobes virtio_gpu,
# virtio_input and evdev, then starts the desktop), /usr/local/bin/ternvale-desktop
# (writes xorg.conf from sysfs, starts Xorg, twm and an xterm), and xwhere, xtrack,
# xclickwait and xname (pointer position, focus and click target, printed on the
# serial console for tests).
#
# macOS cannot build ext4 or run apk, so the image is assembled in a privileged
# linux/arm64 Alpine container (Docker Desktop), like scripts/make-rootfs.sh.
#
# Usage:
#   ./scripts/make-desktop-rootfs.sh [OUT_DIR]
#   SIZE_MIB=1536 ALPINE_VER=3.20 ./scripts/make-desktop-rootfs.sh
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/desktop}"
size_mib="${SIZE_MIB:-1024}"
alpine_ver="${ALPINE_VER:-3.20}"
guest="${script_dir}/desktop-guest"
packages="alpine-baselayout busybox busybox-binsh musl-utils kmod
xorg-server xf86-video-fbdev xf86-input-evdev xkeyboard-config xkbcomp
font-misc-misc font-cursor-misc twm xterm xdotool xprop xsetroot xwd xdpyinfo xwininfo"

log() {
    printf 'make-desktop-rootfs: %s\n' "$*" >&2
}

for tool in docker python3 gzip cpio; do
    command -v "${tool}" >/dev/null 2>&1 || { log "missing required tool: ${tool}"; exit 1; }
done
docker info >/dev/null 2>&1 || { log "docker is not reachable; start Docker Desktop and retry"; exit 1; }
for f in init ternvale-desktop xwhere xclickwait xname xtrack twmrc; do
    [[ -f "${guest}/${f}" ]] || { log "missing ${guest}/${f}"; exit 1; }
done

mkdir -p "${out}"
work="$(mktemp -d "${root}/target/desktop-build.XXXXXX" 2>/dev/null || mktemp -d)"
trap 'rm -rf "${work}"' EXIT
mkdir -p "${work}/kernel"

log "building ${size_mib} MiB ext4 image in alpine:${alpine_ver} (linux/arm64, privileged)"
docker run --rm --privileged --platform linux/arm64 \
    -v "${guest}:/guest:ro" \
    -v "${out}:/out" \
    -v "${work}/kernel:/kernel" \
    -e "SIZE_MIB=${size_mib}" -e "PACKAGES=${packages}" \
    "alpine:${alpine_ver}" \
    sh -euc '
apk add --no-cache e2fsprogs >/dev/null
rm -f /out/rootfs.ext4
dd if=/dev/zero of=/out/rootfs.ext4 bs=1M count="${SIZE_MIB}" status=none
mkfs.ext4 -q -F -L ternvale-desktop /out/rootfs.ext4
mkdir -p /mnt
mount -o loop /out/rootfs.ext4 /mnt
mkdir -p /mnt/etc/apk
cp -r /etc/apk/keys /mnt/etc/apk/
cp /etc/apk/repositories /mnt/etc/apk/repositories
sed -n "s|/main$|/community|p" /etc/apk/repositories >>/mnt/etc/apk/repositories
# shellcheck disable=SC2086
apk --root /mnt --initdb --no-cache add ${PACKAGES} >/tmp/apk.log 2>&1 || { tail -n 30 /tmp/apk.log; exit 1; }
grep -iE "^(ERROR|WARNING)" /tmp/apk.log || true
# The kernel package is unpacked rather than installed: its mkinitfs trigger is not
# wanted, the initramfs is built on the host from these modules.
apk fetch --no-cache -q -o /tmp linux-virt
tar -xzf /tmp/linux-virt-*.apk -C /mnt boot lib/modules 2>/dev/null
kver=$(ls /mnt/lib/modules)
chroot /mnt depmod "${kver}"
chroot /mnt mkfontdir /usr/share/fonts/misc 2>/dev/null || true
mkdir -p /mnt/usr/local/bin /mnt/root
install -m 755 /guest/init /mnt/sbin/init
install -m 755 /guest/ternvale-desktop /guest/xwhere /guest/xclickwait /guest/xname /guest/xtrack /mnt/usr/local/bin/
install -m 644 /guest/twmrc /mnt/root/.twmrc
echo ternvale-desktop >/mnt/etc/hostname
cp /mnt/boot/vmlinuz-virt /mnt/boot/config-* /kernel/
cp -r /mnt/lib/modules/"${kver}" /kernel/modules
cp /mnt/bin/busybox /mnt/lib/ld-musl-aarch64.so.1 /kernel/
echo "kernel ${kver}; $(apk --root /mnt info 2>/dev/null | wc -l) packages; $(du -sm /mnt | cut -f1) MiB used" >/kernel/summary
sync
umount /mnt
e2fsck -fy /out/rootfs.ext4 >/dev/null || [ $? -le 1 ]
'
log "$(cat "${work}/kernel/summary")"

for want in CONFIG_DRM_FBDEV_EMULATION=y CONFIG_FB_DEVICE=y; do
    grep -qx "${want}" "${work}"/kernel/config-* || { log "kernel config lacks ${want}"; exit 1; }
done

log "unpacking vmlinuz-virt to ${out}/Image"
python3 - "${work}/kernel/vmlinuz-virt" "${out}/Image" <<'PY'
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

log "assembling initramfs.cpio"
initrd="${work}/initrd"
mkdir -p "${initrd}/bin" "${initrd}/lib" "${initrd}/modules"
install -m 755 "${work}/kernel/busybox" "${initrd}/bin/busybox"
install -m 755 "${work}/kernel/ld-musl-aarch64.so.1" "${initrd}/lib/ld-musl-aarch64.so.1"
ln -sf ld-musl-aarch64.so.1 "${initrd}/lib/libc.musl-aarch64.so.1"
# ext4 asks the crypto API for "crc32c" at mount time; modules.dep does not say so.
python3 - "${work}/kernel/modules/modules.dep" virtio_mmio virtio_blk crc32c_generic ext4 >"${work}/order" <<'PY'
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
        sys.exit(f"{want} is not a module in this kernel (built in?)")
    visit(by_name[want])
print("\n".join(order))
PY
: >"${initrd}/modules/order"
while read -r rel; do
    name=$(basename "${rel}")
    base="${name%%.ko*}"
    case "${name}" in
        *.ko.gz) gzip -dc "${work}/kernel/modules/${rel}" >"${initrd}/modules/${base}.ko" ;;
        *.ko) cp "${work}/kernel/modules/${rel}" "${initrd}/modules/${base}.ko" ;;
        *) log "unsupported module compression: ${name}"; exit 1 ;;
    esac
    echo "${base}" >>"${initrd}/modules/order"
done <"${work}/order"
log "initramfs modules: $(tr '\n' ' ' <"${initrd}/modules/order")"

cat >"${initrd}/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /newroot
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
for m in $(/bin/busybox cat /modules/order); do
  /bin/busybox insmod "/modules/${m}.ko" || /bin/busybox echo "insmod ${m} failed"
done
i=0
while [ ! -b /dev/vda ] && [ "${i}" -lt 100 ]; do
  /bin/busybox sleep 0.1
  i=$((i + 1))
done
if ! /bin/busybox mount -t ext4 -o rw /dev/vda /newroot; then
  /bin/busybox echo "ternvale desktop initramfs: cannot mount /dev/vda"
  exec /bin/busybox sh -i
fi
/bin/busybox echo "ternvale desktop initramfs: switching to /dev/vda"
/bin/busybox mount --move /proc /newroot/proc
/bin/busybox mount --move /sys /newroot/sys
/bin/busybox mount --move /dev /newroot/dev
exec /bin/busybox switch_root /newroot /sbin/init
INIT
chmod 755 "${initrd}/init"
(
    cd "${initrd}"
    find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc 2>/dev/null
) >"${out}/initramfs.cpio"

cat >"${out}/vm.toml.example" <<EOF
# ternvale run ${out}/vm.toml.example --display window
name = "desktop"
cpus = 2
ram_mib = 1024
kernel = "${out}/Image"
initrd = "${out}/initramfs.cpio"
serial_log = "${out}/serial.log"
cmdline = "console=tty0 console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init"

[[disks]]
path = "${out}/rootfs.ext4"
read_only = false
EOF

log "wrote ${out}/rootfs.ext4 ($(du -h "${out}/rootfs.ext4" | cut -f1)), Image, initramfs.cpio, vm.toml.example"
