#!/bin/bash
# Fetch Red Hat's virtio-win driver ISO for the `[iso] drivers` slot:
#   test-assets/virtio-win/virtio-win-<ver>.iso  (+ virtio-win.iso symlink)
#
# Windows has no inbox virtio drivers. Windows Setup loads `viostor` (virtio-blk)
# and `NetKVM` (virtio-net) from this disc. Ternvale does not bundle it: it is
# Red Hat's redistributable, published by the Fedora virtio-win project
# (https://github.com/virtio-win/virtio-win-pkg-scripts, downloads at
# https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/). Review its
# license terms there before redistributing it yourself.
#
# The project publishes checksums only for its RPMs, so the pinned SHA-256 was
# taken from the download on 2026-10-15 (877373440 bytes, matching the server's
# Content-Length). After the checksum the script checks that the disc carries
# ARM64 `viostor` whose INF matches Ternvale's virtio-blk PCI hardware ID.
#
# Usage:
#   ./scripts/fetch-virtio-win.sh [test-assets/virtio-win]
#
# Override VIRTIO_WIN_VER / VIRTIO_WIN_SHA256 together for another release.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/virtio-win}"
ver="${VIRTIO_WIN_VER:-0.1.302}"
sha="${VIRTIO_WIN_SHA256:-303f7ae40dad495d6ae474fdc571df58958a4dbc5c37a522d80f9a203867949d}"
name="virtio-win-${ver}.iso"
url="https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/archive-virtio/virtio-win-${ver}-1/${name}"
# What Ternvale's virtio-blk function reports (vendor 1af4, device 0x1040 + 2,
# subsystem 1af4:1100, revision 1); see ternvale-vmm/src/pci/identity.rs.
hwid='PCI\VEN_1AF4&DEV_1042&SUBSYS_11001AF4&REV_01'

log() {
    printf 'fetch-virtio-win: %s\n' "$*" >&2
}

mkdir -p "${out}"
iso="${out}/${name}"
if [[ -f "${iso}" ]] && [[ "$(shasum -a 256 "${iso}" | cut -d' ' -f1)" == "${sha}" ]]; then
    log "already have ${iso}"
else
    if [[ ! -f "${iso}.part" ]] || [[ "$(shasum -a 256 "${iso}.part" | cut -d' ' -f1)" != "${sha}" ]]; then
        log "downloading ${url} (about 840 MiB)"
        curl -fL --retry 3 -o "${iso}.part" "${url}"
    fi
    got=$(shasum -a 256 "${iso}.part" | cut -d' ' -f1)
    if [[ "${got}" != "${sha}" ]]; then
        log "sha256 ${got} does not match ${sha}"
        rm -f "${iso}.part"
        exit 1
    fi
    mv "${iso}.part" "${iso}"
fi
log "sha256 ${sha}"

# The w11 driver directories hard-link the 2k25 files, so extract every ARM64 INF.
scratch="${root}/target/virtio-win-check"
mkdir -p "${scratch}"
bsdtar -xf "${iso}" -C "${scratch}" 'viostor/*/ARM64/viostor.inf' 'NetKVM/*/ARM64/netkvm.inf'
inf="${scratch}/viostor/2k25/ARM64/viostor.inf"
if ! LC_ALL=C tr -d '\000' <"${inf}" | grep -qF "${hwid}"; then
    log "${inf} does not list ${hwid}"
    exit 1
fi
log "ARM64 viostor matches ${hwid}"
LC_ALL=C tr -d '\000' <"${inf}" | grep -a '^DriverVer' | sed 's/^/fetch-virtio-win: viostor /' >&2
ln -sf "${name}" "${out}/virtio-win.iso"
ls -l "${iso}" >&2
