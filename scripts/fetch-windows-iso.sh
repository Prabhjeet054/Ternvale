#!/bin/bash
# Fetch the Windows 11 ARM64 installation ISO for the `[iso] installer` slot:
#   test-assets/windows/<name>.iso  (+ windows.iso symlink)
#
# Microsoft does not publish a stable URL. Open
#   https://www.microsoft.com/en-us/software-download/windows11arm64
# choose "Windows 11 (multi-edition ISO for Arm64)", Confirm, pick a language,
# Confirm. The page shows a "Download Now" link (valid 24 hours) and a table of
# SHA-256 hashes per language. Pass both here; the hash is required, so a
# truncated or substituted image is never linked. Ternvale does not bundle the
# ISO; your use of it is governed by Microsoft's license terms.
#
# After the checksum the script checks the disc's boot structure: an El Torito
# boot catalog, the UEFI removable-media loader `efi/boot/bootaa64.efi`, the
# boot manager's BCD store, and the WinPE images `sources/boot.wim` and
# `boot/boot.sdi` it loads. Listing the UDF file system needs `7zz`
# (`brew install sevenzip`).
#
# TODO(verify): on 2026-10-15 the page's "English 64-bit" hash (dbf606ce...)
# did not match the 26300.9457 ISO the same page served (8992f2d2...), while
# 26 random 4 MiB ranges of the download matched the server byte for byte.
# Recheck whether the table has been updated before trusting either value.
#
# Usage:
#   ./scripts/fetch-windows-iso.sh '<Download Now link>' <sha256> [test-assets/windows]
set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "usage: $0 '<Download Now link>' <sha256> [out-dir]" >&2
    exit 2
fi
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
url=$1
sha=$(printf '%s' "$2" | tr 'A-F' 'a-f')
out="${3:-${root}/test-assets/windows}"
name=$(basename "${url%%\?*}")

log() {
    printf 'fetch-windows-iso: %s\n' "$*" >&2
}

if [[ ! "${sha}" =~ ^[0-9a-f]{64}$ ]]; then
    log "sha256 must be 64 hex digits, got '$2'"
    exit 2
fi
if [[ "${name}" != *.iso ]]; then
    log "the link does not name an .iso file: ${name}"
    exit 2
fi

mkdir -p "${out}"
iso="${out}/${name}"
if [[ -f "${iso}" ]] && [[ "$(shasum -a 256 "${iso}" | cut -d' ' -f1)" == "${sha}" ]]; then
    log "already have ${iso}"
else
    got=
    if [[ -f "${iso}.part" ]]; then
        log "hashing the existing ${iso}.part"
        got=$(shasum -a 256 "${iso}.part" | cut -d' ' -f1)
    fi
    if [[ "${got}" != "${sha}" ]]; then
        log "downloading ${name} (about 5-6 GiB; resumes a partial ${name}.part)"
        curl -fL --retry 5 --retry-delay 5 -C - -o "${iso}.part" "${url}"
        log "hashing ${iso}.part"
        got=$(shasum -a 256 "${iso}.part" | cut -d' ' -f1)
    fi
    if [[ "${got}" != "${sha}" ]]; then
        log "sha256 ${got} does not match ${sha}; keeping ${iso}.part for inspection"
        exit 1
    fi
    mv "${iso}.part" "${iso}"
fi
log "sha256 ${sha}"

# El Torito boot record volume descriptor at sector 17 (ECMA-119 8.2,
# "EL TORITO SPECIFICATION" system identifier).
if ! dd if="${iso}" bs=2048 skip=17 count=1 2>/dev/null | LC_ALL=C grep -qa 'EL TORITO SPECIFICATION'; then
    log "${iso}: no El Torito boot record at sector 17"
    exit 1
fi
# The ISO 9660 side of a Windows disc holds only README.TXT; the files are on
# the UDF side (ECMA-167), which bsdtar cannot read. 7-Zip can.
if ! command -v 7zz >/dev/null; then
    log "7zz not found (brew install sevenzip); it reads the disc's UDF file system"
    exit 1
fi
listing=$(7zz l -slt "${iso}" | sed -n 's/^Path = //p' | tr 'A-Z\\' 'a-z/')
for want in efi/boot/bootaa64.efi sources/boot.wim boot/boot.sdi efi/microsoft/boot/bcd; do
    if ! grep -qx "${want}" <<<"${listing}"; then
        log "${iso}: ${want} is missing"
        exit 1
    fi
    log "found ${want}"
done
ln -sf "${name}" "${out}/windows.iso"
ls -l "${iso}" >&2
