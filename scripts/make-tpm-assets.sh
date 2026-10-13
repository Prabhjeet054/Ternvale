#!/bin/bash
# Build test-assets/tpm-linux/{Image,initramfs.cpio} for the uefi-linux-tpm
# boot scenario (tpm2-tools in a Linux guest, CRB -> swtpm).
#
# Kernel: Alpine's arm64 kernels cannot drive the CRB. linux-virt has
# `# CONFIG_TCG_TPM is not set` and linux-lts has `# CONFIG_TCG_CRB is not set`
# (checked for 3.20 and 3.21). Ubuntu 24.04's generic arm64 kernel has
# tpm_crb built in, so this script fetches the pinned cloud-image kernel,
# checks its SHA-256 (from the dated SHA256SUMS), and gunzips it to the raw
# Image (EFI stub, ARM\x64 magic) that EDK2 starts from the ESP.
#
# Userland: Alpine 3.21 (tpm2-tools is not built for aarch64 in 3.20),
# installed with `apk --root` in a linux/arm64 container: busybox, tpm2-tools
# and the device TCTI. /init here is a plain shell; the harness replaces it.
#
# Usage:
#   ./scripts/make-tpm-assets.sh [OUT_DIR]
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/tpm-linux}"
alpine_ver="${ALPINE_VER:-3.21}"
kernel_url="https://cloud-images.ubuntu.com/noble/20260926/unpacked/noble-server-cloudimg-arm64-vmlinuz-generic"
kernel_sha256="71fe6776ef591ea452661a5933e11d2044c5df6aaf76cf39b8d9655fa4e47904"

log() {
    printf 'make-tpm-assets: %s\n' "$*" >&2
}

for tool in curl python3 shasum docker; do
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
out=$(cd "${out}" && pwd)
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

log "downloading ${kernel_url}"
curl -fsSL --retry 3 -A Ternvale -o "${tmp}/vmlinuz" "${kernel_url}"
got=$(shasum -a 256 "${tmp}/vmlinuz" | awk '{print $1}')
if [[ "${got}" != "${kernel_sha256}" ]]; then
    log "vmlinuz sha256 ${got}, expected ${kernel_sha256}"
    exit 1
fi
python3 - "${tmp}/vmlinuz" "${out}/Image" <<'PY'
import gzip, pathlib, sys
blob = pathlib.Path(sys.argv[1]).read_bytes()
image = gzip.decompress(blob) if blob[:2] == b"\x1f\x8b" else blob
if image[56:60] != b"ARM\x64" or image[:2] != b"MZ":
    sys.exit("vmlinuz is not an arm64 Image with an EFI stub")
pathlib.Path(sys.argv[2]).write_bytes(image)
print(f"Image {len(image)} bytes", file=sys.stderr)
PY

cat >"${tmp}/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /tmp
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
/bin/busybox mount -t tmpfs tmp /tmp
exec /bin/busybox sh -i
INIT

log "installing Alpine ${alpine_ver} busybox + tpm2-tools into the initramfs"
docker run --rm --platform linux/arm64 \
    -v "${tmp}/init:/in/init:ro" \
    -v "${out}:/out" \
    -e "ALPINE_VER=${alpine_ver}" \
    "alpine:${alpine_ver}" \
    sh -c '
set -euo pipefail
repo=https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_VER}
apk add --root /rootfs --initdb --no-cache --keys-dir /etc/apk/keys \
    --repository "${repo}/main" --repository "${repo}/community" \
    busybox tpm2-tools tpm2-tss-tcti-device >/dev/null
rm -rf /rootfs/var/cache/apk /rootfs/etc/apk/cache
cp /in/init /rootfs/init
chmod 755 /rootfs/init
chroot /rootfs /usr/bin/tpm2_getrandom --version
cd /rootfs
find . -mindepth 1 | sed "s|^\./||" | sort | cpio -o -H newc >/out/initramfs.cpio 2>/dev/null
'
log "wrote ${out}/Image ($(du -h "${out}/Image" | awk '{print $1}')) and ${out}/initramfs.cpio ($(du -h "${out}/initramfs.cpio" | awk '{print $1}'))"
