#!/bin/bash
# Build test-assets/usb/{Image,initramfs.cpio} for the xHCI + lsusb boot scenario.
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/usb}"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() { printf 'make-usb-initramfs: %s\n' "$*" >&2; }

for tool in curl python3 gzip cpio tar docker; do
    command -v "${tool}" >/dev/null || { log "missing ${tool}"; exit 1; }
done
docker info >/dev/null 2>&1 || { log "docker not reachable"; exit 1; }

mkdir -p "${out}"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

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
    sys.exit(f"{sys.argv[2]} not found")
PY
}

curl -fsSL -A Ternvale -o "${tmp}/apkindex-main.tar.gz" "${alpine}/main/aarch64/APKINDEX.tar.gz"
curl -fsSL -A Ternvale -o "${tmp}/apkindex-community.tar.gz" "${alpine}/community/aarch64/APKINDEX.tar.gz"
tar -xOf "${tmp}/apkindex-main.tar.gz" APKINDEX >"${tmp}/APKINDEX"
tar -xOf "${tmp}/apkindex-community.tar.gz" APKINDEX >>"${tmp}/APKINDEX"

apk_fetch() {
    local pkg="$1"
    local name
    name="$(apk_name "${pkg}")"
    if curl -fsSL -A Ternvale -o "${tmp}/${pkg}.apk" "${alpine}/main/aarch64/${name}"; then
        return 0
    fi
    curl -fsSL -A Ternvale -o "${tmp}/${pkg}.apk" "${alpine}/community/aarch64/${name}"
}

for pkg in linux-virt busybox musl usbutils libusb eudev-libs; do
    apk_fetch "${pkg}"
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
[[ -n "${mod_dir}" ]] || { log "modules.dep not found"; exit 1; }
tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/modules.dep" >"${tmp}/modules.dep"
kver=$(basename "${mod_dir}")
linux_src_ver="${kver%%-*}"
log "building xhci-plat-hcd.ko for ${kver} (not shipped in linux-virt; needed for generic-xhci DT)"
docker run --rm --platform linux/arm64 -e "KVER=${kver}" -e "LINUX_SRC_VER=${linux_src_ver}" "alpine:${alpine_ver}" sh -c '
set -e
apk add --no-cache linux-virt-dev build-base curl xz >/dev/null
KDIR="/usr/src/linux-headers-${KVER}"
cd /tmp
curl -fsSL -o linux.tar.xz "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-${LINUX_SRC_VER}.tar.xz"
tar -xf linux.tar.xz "linux-${LINUX_SRC_VER}/drivers/usb/host"
cd "linux-${LINUX_SRC_VER}/drivers/usb/host"
cat > Makefile <<EOF
obj-m := xhci-plat-hcd.o
xhci-plat-hcd-y := xhci-plat.o
EOF
make ARCH=arm64 -C "$KDIR" M="$PWD" modules >/dev/null
cat xhci-plat-hcd.ko
' >"${tmp}/xhci-plat-hcd.ko"
mkdir -p "${tmp}/initrd/modules" "${tmp}/initrd/bin" "${tmp}/initrd/lib"
tar -xOf "${tmp}/busybox.apk" bin/busybox >"${tmp}/initrd/bin/busybox"
chmod 755 "${tmp}/initrd/bin/busybox"
tar -xOf "${tmp}/musl.apk" lib/ld-musl-aarch64.so.1 >"${tmp}/initrd/lib/ld-musl-aarch64.so.1"
ln -sf ld-musl-aarch64.so.1 "${tmp}/initrd/lib/libc.musl-aarch64.so.1"
docker run --rm --platform linux/arm64 -v "${tmp}/initrd/lib:/out" "alpine:${alpine_ver}" sh -c '
set -e
apk add --no-cache libusb eudev-libs >/dev/null
cp -a /usr/lib/libusb-1.0.so* /out/
cp -a /lib/libudev.so* /out/ 2>/dev/null || cp -a /usr/lib/libudev.so* /out/
'
tar -xOf "${tmp}/usbutils.apk" usr/bin/lsusb >"${tmp}/initrd/bin/lsusb"
chmod 755 "${tmp}/initrd/bin/busybox" "${tmp}/initrd/bin/lsusb" \
    "${tmp}/initrd/lib/ld-musl-aarch64.so.1" "${tmp}/initrd/lib/"*.so* 2>/dev/null || true
find "${tmp}/initrd/lib" -type f -empty -delete
(
    cd "${tmp}/initrd/lib"
    for lib in lib*.so.*.*; do
        [[ -s "${lib}" ]] || continue
        base="${lib%%.so.*}.so"
        minor="${lib#${base}.}"
        ln -sf "${lib}" "${base}.${minor%%.*}"
    done
)

python3 - "${tmp}/modules.dep" >"${tmp}/order" <<'PY'
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

for want in ("xhci_hcd", "xhci_plat_hcd", "usbcore", "usbhid", "hid"):
    if want not in by_name:
        continue
    visit(by_name[want])
for path in order:
    print(path)
PY
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
cp "${tmp}/xhci-plat-hcd.ko" "${tmp}/initrd/modules/xhci-plat-hcd.ko"
printf '%s\n' "xhci-plat-hcd" >>"${tmp}/initrd/modules/order"
log "module xhci-plat-hcd (out-of-tree)"

cat >"${tmp}/initrd/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /tmp /sbin /usr/bin /usr/sbin
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
/bin/busybox --install -s
for m in $(cat /modules/order); do
  insmod /modules/${m}.ko 2>/dev/null || echo "insmod ${m} failed"
done
sleep 1
lsusb > /tmp/lsusb.txt 2>&1 || true
cat /tmp/lsusb.txt
if grep -qi 'xhci' /tmp/lsusb.txt && grep -qi '0627' /tmp/lsusb.txt; then
  echo "ternvale usb ready xhci=yes keyboard=0627"
else
  echo "ternvale usb probe failed"
  exit 1
fi
exec setsid sh -c 'exec sh -i' </dev/console >/dev/console 2>&1
INIT
chmod 755 "${tmp}/initrd/init"
( cd "${tmp}/initrd" && find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc ) >"${out}/initramfs.cpio"
log "wrote ${out}/Image and ${out}/initramfs.cpio"
