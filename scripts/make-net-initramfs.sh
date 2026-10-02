#!/bin/bash
# Build test-assets/virtio-net/{Image,initramfs.cpio} for the virtio-net boot scenario.
#
# The kernel Image and the modules come from the same Alpine linux-virt package,
# so insmod never sees a version mismatch. Alpine's virt kernel builds
# virtio_mmio and virtio_net as modules; the dependency order is read from the
# package's modules.dep. The initramfs /init loads them, installs the busybox
# applets (ip, ping), waits for eth0, prints "ternvale net ready", and starts a
# shell. No Docker or root needed.
#
# Usage:
#   ./scripts/make-net-initramfs.sh [OUT_DIR]
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/virtio-net}"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() {
    printf 'make-net-initramfs: %s\n' "$*" >&2
}

for tool in curl python3 gzip cpio tar; do
    command -v "${tool}" >/dev/null 2>&1 || {
        log "missing required tool: ${tool}"
        exit 1
    }
done

mkdir -p "${out}"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

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
    curl -fL --retry 3 -A Ternvale -o "${tmp}/${pkg}.apk" "${alpine}/main/aarch64/${name}"
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

log "resolving virtio_mmio and virtio_net module order"
tar -tf "${tmp}/linux-virt.apk" >"${tmp}/files"
mod_dir=$(python3 - "${tmp}/files" <<'PY'
import sys
for line in open(sys.argv[1]):
    line = line.strip()
    if line.endswith("/modules.dep"):
        print(line.rsplit("/", 1)[0])
        break
else:
    sys.exit("modules.dep not found in linux-virt")
PY
)
tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/modules.dep" >"${tmp}/modules.dep"
python3 - "${tmp}/modules.dep" virtio_mmio virtio_net >"${tmp}/order" <<'PY'
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
        sys.exit(f"{want} is not a module in this kernel (built in?)")
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
while [ ! -e /sys/class/net/eth0 ] && [ "$i" -lt 100 ]; do
  sleep 0.1
  i=$((i + 1))
done
if [ -e /sys/class/net/eth0 ]; then
  echo "ternvale net ready mac=$(cat /sys/class/net/eth0/address)"
else
  echo "eth0 missing"
fi
exec setsid sh -c 'exec sh -i' </dev/console >/dev/console 2>&1
INIT
chmod 755 "${tmp}/initrd/init"

(
    cd "${tmp}/initrd"
    find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc
) >"${out}/initramfs.cpio"

log "kernel modules dir ${mod_dir}"
log "wrote ${out}/Image and ${out}/initramfs.cpio"
