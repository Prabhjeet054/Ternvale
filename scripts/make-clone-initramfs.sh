#!/bin/bash
# Build test-assets/clone-boot/initramfs.cpio for the linked-clone boot scenario.
#
# Guest mounts /dev/vda (ext4), writes /ternvale-clone-<id>.txt from ternvale.clone_id=
# on the cmdline, then powers off. With ternvale.clone_verify=1 it checks that only its
# marker file exists on the disk.
#
# Reuses the kernel Image from test-assets/virtio-root (run ./scripts/make-rootfs.sh first).
#
# Usage:
#   ./scripts/make-clone-initramfs.sh [OUT_DIR]
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/clone-boot}"
virtio_root="${root}/test-assets/virtio-root"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() {
    printf 'make-clone-initramfs: %s\n' "$*" >&2
}

if [[ ! -f "${virtio_root}/Image" ]]; then
    log "missing ${virtio_root}/Image (run ./scripts/make-rootfs.sh)"
    exit 1
fi

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
    curl -fsSL --retry 3 -A Ternvale -o "${tmp}/${pkg}.apk" "${alpine}/main/aarch64/${name}"
done

tar -tf "${tmp}/linux-virt.apk" >"${tmp}/files"
mod_dir=$(grep -m1 '/modules.dep$' "${tmp}/files" | sed 's|/modules.dep$||')
tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/modules.dep" >"${tmp}/modules.dep"
python3 - "${tmp}/modules.dep" virtio_mmio virtio_blk crc16 crc32c_generic libcrc32c mbcache jbd2 ext4 >"${tmp}/order" <<'PY'
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
    case "${name}" in
        *.ko.gz) tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/${rel}" | gzip -dc >"${tmp}/initrd/modules/${base}.ko" ;;
        *.ko) tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/${rel}" >"${tmp}/initrd/modules/${base}.ko" ;;
    esac
    printf '%s\n' "${base}" >>"${tmp}/initrd/modules/order"
done <"${tmp}/order"

cat >"${tmp}/initrd/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /mnt
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
/bin/busybox --install -s
for m in $(cat /modules/order); do
  insmod /modules/${m}.ko || echo "insmod ${m} failed"
done
i=0
while [ ! -b /dev/vda ] && [ "$i" -lt 150 ]; do
  sleep 0.1
  i=$((i + 1))
done
if ! /bin/busybox mount -t ext4 -o rw /dev/vda /mnt; then
  echo "ternvale clone mount failed"
  exit 1
fi
id=$(sed -n 's/.*ternvale\.clone_id=\([^ ]*\).*/\1/p' /proc/cmdline)
verify=$(sed -n 's/.*ternvale\.clone_verify=\([^ ]*\).*/\1/p' /proc/cmdline)
if [ -z "$id" ]; then
  echo "ternvale clone missing ternvale.clone_id="
  exit 1
fi
marker="TERNVALE_CLONE_FILE_${id}"
if [ "$verify" = "1" ]; then
  for other in a b c; do
    if [ "$other" = "$id" ]; then
      continue
    fi
    if [ -f "/mnt/ternvale-clone-${other}.txt" ]; then
      echo "ternvale clone verify saw foreign file ${other}"
      exit 1
    fi
  done
  if [ ! -f "/mnt/ternvale-clone-${id}.txt" ]; then
    echo "ternvale clone verify missing own file"
    exit 1
  fi
  echo "ternvale clone verify ${id} ok"
else
  printf '%s\n' "$marker" >"/mnt/ternvale-clone-${id}.txt"
  sync
  echo "ternvale clone ${id} done"
fi
exec /bin/busybox poweroff -f
INIT
chmod 755 "${tmp}/initrd/init"

(
    cd "${tmp}/initrd"
    find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc
) >"${out}/initramfs.cpio"

log "wrote ${out}/initramfs.cpio (kernel: ${virtio_root}/Image)"
