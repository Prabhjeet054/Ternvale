#!/bin/bash
# Build the Step 62 Alpine arm64 benchmark guest (sysbench, fio, iperf3, STREAM).
# See scripts/bench/README.md. Output: test-assets/bench/{Image,initramfs.cpio,rootfs.ext4}
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/../.." && pwd)
out="${1:-${root}/test-assets/bench}"
size_mib="${SIZE_MIB:-1024}"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() { printf 'bench-image: %s\n' "$*" >&2; }

need() {
    command -v "$1" >/dev/null 2>&1 || { log "missing $1"; exit 1; }
}
need docker
need curl
need python3
need gzip
need cpio
docker info >/dev/null 2>&1 || { log "docker is not reachable"; exit 1; }

mkdir -p "${out}"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

log "downloading Alpine aarch64 minirootfs"
miniroot_url=$(curl -fsSL "${alpine}/releases/aarch64/" | python3 -c "
import re,sys
html=sys.stdin.read()
m=re.findall(r'alpine-minirootfs-[0-9.]+-aarch64\\.tar\\.gz', html)
print(sorted(set(m))[-1] if m else '')
")
[[ -n "${miniroot_url}" ]] || { log "no minirootfs listing on ${alpine}"; exit 1; }
curl -fL --retry 3 -A Ternvale -o "${tmp}/miniroot.tar.gz" \
    "${alpine}/releases/aarch64/${miniroot_url}"

mkdir -p "${tmp}/overlay/sbin" "${tmp}/overlay/root"
cp "${script_dir}/guest-bench.sh" "${tmp}/overlay/root/guest-bench.sh"
chmod 755 "${tmp}/overlay/root/guest-bench.sh"

cat >"${tmp}/overlay/sbin/init" <<'INIT'
#!/bin/sh
/bin/busybox mkdir -p /proc /sys /dev /tmp /run /var/tmp
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev 2>/dev/null || true
/bin/busybox --install -s 2>/dev/null || true
echo BENCH_LOGIN
exec /bin/busybox setsid /bin/busybox sh -c 'exec /bin/busybox sh -i' \
    </dev/console >/dev/console 2>&1
INIT
chmod 755 "${tmp}/overlay/sbin/init"

log "building ${size_mib} MiB ext4 image (sysbench/fio/iperf3/stream) via Docker"
docker run --rm --privileged --platform linux/arm64 \
    -v "${tmp}/miniroot.tar.gz:/in/miniroot.tar.gz:ro" \
    -v "${tmp}/overlay:/overlay:ro" \
    -v "${script_dir}/stream.c:/in/stream.c:ro" \
    -v "${out}:/out" \
    -e "SIZE_MIB=${size_mib}" \
    -e "ALPINE_VER=${alpine_ver}" \
    "alpine:${alpine_ver}" \
    sh -c '
set -euo pipefail
apk add --no-cache e2fsprogs curl gcc musl-dev >/dev/null
dd if=/dev/zero of=/out/rootfs.ext4 bs=1M count="${SIZE_MIB}" status=none
mkfs.ext4 -F -L ternvale-bench /out/rootfs.ext4 >/dev/null
mkdir -p /mnt
mount -o loop /out/rootfs.ext4 /mnt
tar -xzf /in/miniroot.tar.gz -C /mnt
cp /overlay/sbin/init /mnt/sbin/init
chmod 755 /mnt/sbin/init
cp /overlay/root/guest-bench.sh /mnt/root/guest-bench.sh
chmod 755 /mnt/root/guest-bench.sh
mkdir -p /mnt/proc /mnt/sys /mnt/dev /mnt/tmp /mnt/run /mnt/var/tmp
cp /etc/resolv.conf /mnt/etc/resolv.conf
echo "https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_VER}/main" > /mnt/etc/apk/repositories
echo "https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_VER}/community" >> /mnt/etc/apk/repositories
apk add --root /mnt --no-cache sysbench fio iperf3 python3
gcc -O3 -DSTREAM_ARRAY_SIZE=10000000 -o /mnt/usr/bin/stream /in/stream.c
chmod 755 /mnt/usr/bin/stream
sync
umount /mnt
e2fsck -fy /out/rootfs.ext4 >/dev/null
'

log "fetching linux-virt for Image + initramfs modules"
curl -fsSL --retry 3 -A Ternvale -o "${tmp}/apkindex.tar.gz" \
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
for pkg in linux-virt busybox musl; do
    name="$(apk_name "${pkg}")"
    log "downloading ${name}"
    curl -fL --retry 3 -A Ternvale -o "${tmp}/${pkg}.apk" "${alpine}/main/aarch64/${name}"
done

log "unpacking vmlinuz-virt to ${out}/Image"
tar -xOf "${tmp}/linux-virt.apk" boot/vmlinuz-virt >"${tmp}/vmlinuz"
python3 - "${tmp}/vmlinuz" "${out}/Image" <<'PY'
import gzip, pathlib, struct, sys
blob = pathlib.Path(sys.argv[1]).read_bytes()
if blob[56:60] == b"ARM\x64":
    image = blob
elif blob[4:8] == b"zimg":
    off, size = struct.unpack_from("<II", blob, 8)
    comp = blob[24:32].split(b"\x00", 1)[0]
    if comp != b"gzip":
        sys.exit(f"unsupported zboot compression {comp!r}")
    image = gzip.decompress(blob[off:off + size])
else:
    sys.exit("vmlinuz-virt is neither a raw arm64 Image nor an EFI zboot image")
if image[56:60] != b"ARM\x64":
    sys.exit("unpacked file is not an arm64 Image")
pathlib.Path(sys.argv[2]).write_bytes(image)
PY

tar -tf "${tmp}/linux-virt.apk" >"${tmp}/files"
mod_dir=$(python3 - "${tmp}/files" <<'PY'
import sys
for line in open(sys.argv[1]):
    line = line.strip()
    if line.endswith("/modules.dep"):
        print(line.rsplit("/", 1)[0])
        break
else:
    sys.exit("modules.dep not found")
PY
)
tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/modules.dep" >"${tmp}/modules.dep"
python3 - "${tmp}/modules.dep" virtio_mmio virtio_blk virtio_net ext4 crc32c_generic libcrc32c crc16 mbcache jbd2 >"${tmp}/order" <<'PY'
import sys
deps = {}
for line in open(sys.argv[1]):
    if ":" not in line:
        continue
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
        continue
    visit(by_name[want])
print("\n".join(order))
PY

mkdir -p "${tmp}/initrd/bin" "${tmp}/initrd/lib" "${tmp}/initrd/modules"
tar -xOf "${tmp}/busybox.apk" bin/busybox >"${tmp}/initrd/bin/busybox"
tar -xOf "${tmp}/musl.apk" lib/ld-musl-aarch64.so.1 >"${tmp}/initrd/lib/ld-musl-aarch64.so.1"
chmod 755 "${tmp}/initrd/bin/busybox" "${tmp}/initrd/lib/ld-musl-aarch64.so.1"
ln -sf ld-musl-aarch64.so.1 "${tmp}/initrd/lib/libc.musl-aarch64.so.1"
: >"${tmp}/initrd/modules/order"
while read -r rel; do
    name=$(basename "${rel}")
    base="${name%%.ko*}"
    member="${mod_dir}/${rel}"
    case "${name}" in
        *.ko.gz) tar -xOf "${tmp}/linux-virt.apk" "${member}" | gzip -dc >"${tmp}/initrd/modules/${base}.ko" ;;
        *.ko) tar -xOf "${tmp}/linux-virt.apk" "${member}" >"${tmp}/initrd/modules/${base}.ko" ;;
        *) log "unsupported module: ${name}"; exit 1 ;;
    esac
    printf '%s\n' "${base}" >>"${tmp}/initrd/modules/order"
done <"${tmp}/order"

cat >"${tmp}/initrd/init" <<'INITRD'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /newroot
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
for m in $(/bin/busybox cat /modules/order); do
  /bin/busybox insmod /modules/${m}.ko || /bin/busybox echo "insmod ${m} failed"
done
i=0
while [ ! -e /sys/block/vda ] && [ "$i" -lt 100 ]; do
  /bin/busybox sleep 0.1
  i=$((i + 1))
done
if [ -e /sys/block/vda/dev ] && [ ! -b /dev/vda ]; then
  majmin=$(/bin/busybox cat /sys/block/vda/dev)
  /bin/busybox mknod /dev/vda b "${majmin%:*}" "${majmin#*:}"
fi
/bin/busybox echo "vda ready"
if ! /bin/busybox mount -t ext4 -o rw /dev/vda /newroot; then
  /bin/busybox echo "mount /dev/vda failed"
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

log "wrote ${out}/rootfs.ext4 ($(du -h "${out}/rootfs.ext4" | awk '{print $1}'))"
log "wrote ${out}/initramfs.cpio ($(du -h "${out}/initramfs.cpio" | awk '{print $1}'))"
log "wrote ${out}/Image ($(du -h "${out}/Image" | awk '{print $1}'))"
log "done"
