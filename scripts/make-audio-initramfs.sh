#!/bin/bash
# Build test-assets/virtio-audio/{Image,initramfs.cpio} for the virtio-sound boot scenario.
#
# Loads virtio_mmio + virtio_snd (Alpine linux-virt), waits for /dev/snd/pcmC0D0p, runs
# speaker-test (440 Hz sine, ~1 s), prints "ternvale audio ready" and "audio-check done".
#
# Usage:
#   ./scripts/make-audio-initramfs.sh [OUT_DIR]
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/virtio-audio}"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() {
    printf 'make-audio-initramfs: %s\n' "$*" >&2
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
for pkg in linux-virt busybox musl alsa-lib alsa-utils; do
    name="$(apk_name "${pkg}")"
    log "downloading ${name}"
    curl -fsSL --retry 3 -A Ternvale -o "${tmp}/${pkg}.apk" "${alpine}/main/aarch64/${name}"
done

tar -tf "${tmp}/linux-virt.apk" >"${tmp}/files"
config=$(grep -m1 '^boot/config-' "${tmp}/files" || true)
[[ -n "${config}" ]] || { log "kernel config not found in linux-virt"; exit 1; }
tar -xOf "${tmp}/linux-virt.apk" "${config}" >"${tmp}/config"
grep -qx 'CONFIG_SND_VIRTIO=m' "${tmp}/config" || {
    log "${config} lacks CONFIG_SND_VIRTIO=m"
    exit 1
}

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
python3 - "${tmp}/modules.dep" virtio_mmio virtio_snd >"${tmp}/order" <<'PY'
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

mkdir -p "${tmp}/initrd/bin" "${tmp}/initrd/lib" "${tmp}/initrd/modules" "${tmp}/initrd/usr/bin"
tar -xOf "${tmp}/busybox.apk" bin/busybox >"${tmp}/initrd/bin/busybox"
tar -xOf "${tmp}/musl.apk" lib/ld-musl-aarch64.so.1 >"${tmp}/initrd/lib/ld-musl-aarch64.so.1"
chmod 755 "${tmp}/initrd/bin/busybox" "${tmp}/initrd/lib/ld-musl-aarch64.so.1"
ln -sf ld-musl-aarch64.so.1 "${tmp}/initrd/lib/libc.musl-aarch64.so.1"

tar -xOf "${tmp}/alsa-utils.apk" usr/bin/speaker-test >"${tmp}/initrd/usr/bin/speaker-test"
chmod 755 "${tmp}/initrd/usr/bin/speaker-test"
docker run --rm --platform linux/arm64 -v "${tmp}/initrd/lib:/out" "alpine:${alpine_ver}" sh -c '
set -e
apk add --no-cache alsa-lib >/dev/null
cp -a /usr/lib/libasound.so* /out/
'
(
    cd "${tmp}/initrd/lib"
    for lib in libasound.so.*.*; do
        [[ -f "${lib}" ]] || continue
        base="${lib%%.so.*}.so"
        ln -sf "${lib}" "${base}.${lib#${base}.}"
    done
)

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

cat >"${tmp}/initrd/bin/audio-check" <<'CHECK'
#!/bin/busybox sh
# One short 440 Hz sine on the default ALSA PCM (virtio-sound card 0).
export PATH=/usr/bin:/bin
if [ ! -e /dev/snd/pcmC0D0p ]; then
  echo "audio-check missing pcmC0D0p"
  exit 1
fi
card=$(sed -n '1s/^[[:space:]]*0[[:space:]]*\[\(.*\)\].*/\1/p' /proc/asound/cards)
echo "audio-check card=${card:-unknown} pcm=pcmC0D0p"
/usr/bin/speaker-test -D default -t sine -f 440 -r 48000 -c 2 -s 1 -l 1 </dev/null
echo "audio-check done"
CHECK
chmod 755 "${tmp}/initrd/bin/audio-check"

cat >"${tmp}/initrd/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /sbin /usr/bin /usr/sbin /bin
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
/bin/busybox --install -s
for m in $(cat /modules/order); do
  insmod /modules/${m}.ko || echo "insmod ${m} failed"
done
i=0
while [ ! -e /dev/snd/pcmC0D0p ] && [ "$i" -lt 150 ]; do
  sleep 0.1
  i=$((i + 1))
done
card=$(head -n1 /proc/asound/cards 2>/dev/null | sed 's/^[[:space:]]*//')
if [ -e /dev/snd/pcmC0D0p ]; then
  echo "ternvale audio ready pcm=yes card=${card:-unknown}"
  /bin/audio-check || { echo "audio-check failed"; exit 1; }
else
  echo "ternvale audio probe failed card=${card:-missing}"
  exit 1
fi
exec /bin/busybox poweroff -f
INIT
chmod 755 "${tmp}/initrd/init"

(
    cd "${tmp}/initrd"
    find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc
) >"${out}/initramfs.cpio"

log "wrote ${out}/Image and ${out}/initramfs.cpio"
