#!/bin/bash
# Fetch the arm64 installer ISO the `installer` boot scenario attaches as a virtio disk:
#   test-assets/installer/alpine-virt-<ver>-aarch64.iso
#
# Alpine's "virt" flavour is the smallest arm64 installer (~70 MiB). Its El Torito EFI
# image holds GRUB (efi/boot/bootaa64.efi), which loads boot/vmlinuz-virt and
# boot/initramfs-virt; the live system then runs the `setup-alpine` installer.
#
# Usage:
#   ./scripts/fetch-installer-iso.sh [test-assets/installer]
#
# Override ALPINE_ISO_VER / ALPINE_ISO_SHA256 together for another release.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/installer}"
ver="${ALPINE_ISO_VER:-3.20.10}"
branch="v${ver%.*}"
sha="${ALPINE_ISO_SHA256:-12aa4b6f6e96cfff19bac09f9efa501fd404f4275d7653bb6e737cbbe280b59f}"
name="alpine-virt-${ver}-aarch64.iso"
url="https://dl-cdn.alpinelinux.org/alpine/${branch}/releases/aarch64/${name}"

log() {
    printf 'fetch-installer-iso: %s\n' "$*" >&2
}

mkdir -p "${out}"
iso="${out}/${name}"
if [[ -f "${iso}" ]] && [[ "$(shasum -a 256 "${iso}" | cut -d' ' -f1)" == "${sha}" ]]; then
    log "already have ${iso}"
else
    log "downloading ${url}"
    curl -fL --retry 3 -o "${iso}.part" "${url}"
    got=$(shasum -a 256 "${iso}.part" | cut -d' ' -f1)
    if [[ "${got}" != "${sha}" ]]; then
        log "sha256 ${got} does not match ${sha}"
        rm -f "${iso}.part"
        exit 1
    fi
    mv "${iso}.part" "${iso}"
fi
log "sha256 ${sha}"
ln -sf "${name}" "${out}/installer.iso"
ls -l "${iso}" >&2
