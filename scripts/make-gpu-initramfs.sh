#!/bin/bash
# Build test-assets/virtio-gpu/{Image,initramfs.cpio} for the virtio-gpu boot scenario.
#
# Like make-devices-initramfs.sh, the kernel Image and modules come from one Alpine
# linux-virt package (6.6: CONFIG_DRM_VIRTIO_GPU=m, CONFIG_DRM_FBDEV_EMULATION=y,
# CONFIG_FB=m with CONFIG_FB_DEVICE=y), and the load order of virtio_mmio and virtio-gpu
# (drm, drm_kms_helper, drm_shmem_helper, fb, ...) comes from its modules.dep. Nothing is
# compiled, so no Docker is needed.
#
# The initramfs /init loads the modules, waits for /sys/class/graphics/fb0, prints
# "ternvale gpu ready fb0=<name>", and starts a shell. /bin/gpu-check (run by the
# harness, so each serial send stays within the 16-byte PL011 FIFO) prints the fb0 sysfs
# attributes and `fbset`, unbinds fbcon so nothing else draws, writes two known rows
# through /dev/fb0 (rows 300 and 301), and prints "gpu-check done".
#
# The same initramfs serves the virtio-input scenario (Step 45): virtio_input and evdev
# are loaded too, and /bin/input-check lists every evdev device ("input eventN name=...
# ev=... key=... abs=... rel=..."), reads 8 events from the keyboard and 9 from the
# tablet (found by name) while the host injects them after "input-check ready", and
# dumps the raw 24-byte input_event records with `od` between "input-check keyboard",
# "input-check tablet" and "input-check done".
#
# With `ternvale.getty` on the kernel command line, /init also keeps a busybox getty on
# tty1 (the framebuffer console). Its login program, /bin/ternvale-login, prints
# "ternvale getty login name=<typed name>" on the serial console and starts a shell on
# tty1, so keyboard input from the viewer is visible on both. /bin/pointer-check N dumps
# the next N tablet events like input-check does.
#
# Usage:
#   ./scripts/make-gpu-initramfs.sh [OUT_DIR]
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/virtio-gpu}"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() {
    printf 'make-gpu-initramfs: %s\n' "$*" >&2
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
    curl -fsSL --retry 3 -A Ternvale -o "${tmp}/${pkg}.apk" "${alpine}/main/aarch64/${name}"
done

tar -tf "${tmp}/linux-virt.apk" >"${tmp}/files"
config=$(grep -m1 '^boot/config-' "${tmp}/files" || true)
[[ -n "${config}" ]] || { log "kernel config not found in linux-virt"; exit 1; }
tar -xOf "${tmp}/linux-virt.apk" "${config}" >"${tmp}/config"
for want in CONFIG_DRM_FBDEV_EMULATION=y CONFIG_FB_DEVICE=y; do
    grep -qx "${want}" "${tmp}/config" || { log "${config} lacks ${want}"; exit 1; }
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

mod_dir=$(grep -m1 '/modules.dep$' "${tmp}/files" | sed 's|/modules.dep$||')
[[ -n "${mod_dir}" ]] || { log "modules.dep not found in linux-virt"; exit 1; }
tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/modules.dep" >"${tmp}/modules.dep"
python3 - "${tmp}/modules.dep" virtio_mmio virtio_gpu virtio_input evdev >"${tmp}/order" <<'PY'
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
        *) log "unsupported module compression: ${name}"; exit 1 ;;
    esac
    printf '%s\n' "${base}" >>"${tmp}/initrd/modules/order"
    log "module ${base}"
done <"${tmp}/order"

cat >"${tmp}/initrd/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /sbin /usr/bin /usr/sbin /tmp
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
/bin/busybox --install -s
for m in $(cat /modules/order); do
  insmod /modules/${m}.ko || echo "insmod ${m} failed"
done
i=0
while [ ! -e /sys/class/graphics/fb0 ] && [ "$i" -lt 100 ]; do
  sleep 0.1
  i=$((i + 1))
done
echo "ternvale gpu ready fb0=$(cat /sys/class/graphics/fb0/name 2>/dev/null || echo missing)"
case " $(cat /proc/cmdline) " in
  *" ternvale.getty "*)
    (while :; do setsid /bin/busybox getty -l /bin/ternvale-login 0 tty1 linux; sleep 1; done) &
    echo "ternvale getty on tty1"
    ;;
esac
exec setsid sh -c 'exec sh -i' </dev/console >/dev/console 2>&1
INIT
chmod 755 "${tmp}/initrd/init"

# Row 300 is B,G,R,X = 11 22 33 ff and row 301 is 44 55 66 ff in every pixel; the
# harness checks both rows in the host frame and that rows 299 and 302 differ.
cat >"${tmp}/initrd/bin/gpu-check" <<'CHECK'
#!/bin/busybox sh
fb=/sys/class/graphics/fb0
for attr in name virtual_size bits_per_pixel stride; do
  echo "fb0 ${attr}=$(cat ${fb}/${attr})"
done
fbset -fb /dev/fb0
for vt in /sys/class/vtconsole/vtcon*; do
  if grep -q 'frame buffer' "${vt}/name"; then
    echo 0 >"${vt}/bind" && echo "unbound $(cat ${vt}/name)"
  fi
done
stride=$(cat ${fb}/stride)
printf '\021\042\063\377' >/tmp/a
printf '\104\125\146\377' >/tmp/b
n=4
while [ "${n}" -lt "${stride}" ]; do
  cat /tmp/a /tmp/a >/tmp/t && mv /tmp/t /tmp/a
  cat /tmp/b /tmp/b >/tmp/t && mv /tmp/t /tmp/b
  n=$((n * 2))
done
{ head -c "${stride}" /tmp/a; head -c "${stride}" /tmp/b; } >/tmp/rows
# One write() for both rows: Linux 6.6 drm_fb_helper_memory_range_to_clip() turns a
# write of exactly one whole scanline into a zero-width damage clip (x2 = 0), and
# fbdev-generic then never blits or flushes it.
dd if=/tmp/rows of=/dev/fb0 bs=$((stride * 2)) seek=150 count=1 2>/dev/null
echo "gpu-check wrote rows 300-301 rc=$? bytes=$(wc -c </tmp/rows)"
sleep 1
echo "gpu-check readback row 301: $(dd if=/dev/fb0 bs="${stride}" skip=301 count=1 2>/dev/null | head -c 8 | hexdump -v -e '8/1 "%02x"')"
echo "gpu-check done"
CHECK
chmod 755 "${tmp}/initrd/bin/gpu-check"

# Event counts match the reports the boot harness (tests/boot/input.rs) injects.
cat >"${tmp}/initrd/bin/input-check" <<'CHECK'
#!/bin/busybox sh
kbd=""
tab=""
for d in /sys/class/input/event*; do
  e=$(basename "${d}")
  c="${d}/device/capabilities"
  n=$(cat "${d}/device/name")
  echo "input ${e} name=${n} ev=$(cat ${c}/ev) key=$(cat ${c}/key | tr ' ' '_') abs=$(cat ${c}/abs) rel=$(cat ${c}/rel)"
  case "${n}" in
    *Keyboard*) kbd=/dev/input/${e} ;;
    *Tablet*) tab=/dev/input/${e} ;;
  esac
done
if [ -z "${kbd}" ] || [ -z "${tab}" ]; then
  echo "input-check missing kbd=${kbd} tab=${tab}"
  echo "input-check done"
  exit 1
fi
dd if="${kbd}" of=/tmp/kbd bs=24 count=8 2>/dev/null &
k=$!
dd if="${tab}" of=/tmp/tab bs=24 count=9 2>/dev/null &
t=$!
sleep 1
echo "input-check ready"
wait ${k}
wait ${t}
echo "input-check keyboard"
od -An -tx1 -v /tmp/kbd
echo "input-check tablet"
od -An -tx1 -v /tmp/tab
echo "input-check done"
CHECK
chmod 755 "${tmp}/initrd/bin/input-check"

# getty runs this as `ternvale-login -- <name>` once a login name is typed on tty1.
cat >"${tmp}/initrd/bin/ternvale-login" <<'LOGIN'
#!/bin/busybox sh
for arg in "$@"; do name="${arg}"; done
echo "ternvale getty login name=${name}" >/dev/ttyAMA0
echo "Welcome to Ternvale, ${name}"
exec sh -i
LOGIN
chmod 755 "${tmp}/initrd/bin/ternvale-login"

# Prints "pointer-check ready", then dumps the next N tablet events (raw 24-byte
# input_event records) between "pointer-check events" and "pointer-check done".
cat >"${tmp}/initrd/bin/pointer-check" <<'CHECK'
#!/bin/busybox sh
tab=""
for d in /sys/class/input/event*; do
  case "$(cat ${d}/device/name)" in *Tablet*) tab=/dev/input/$(basename ${d}) ;; esac
done
[ -n "${tab}" ] || { echo "pointer-check no tablet"; echo "pointer-check done"; exit 1; }
timeout 60 dd if="${tab}" of=/tmp/ptr bs=24 count="${1:-3}" 2>/dev/null &
p=$!
sleep 1
echo "pointer-check ready"
wait ${p}
echo "pointer-check events"
od -An -tx1 -v /tmp/ptr
echo "pointer-check done"
CHECK
chmod 755 "${tmp}/initrd/bin/pointer-check"

(
    cd "${tmp}/initrd"
    find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc
) >"${out}/initramfs.cpio"

log "kernel config ${config}, modules dir ${mod_dir}"
log "wrote ${out}/Image and ${out}/initramfs.cpio"
