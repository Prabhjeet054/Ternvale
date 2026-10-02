#!/bin/bash
# Fetch the EDK2 AArch64 firmware the `firmware` boot scenario uses:
#   test-assets/firmware/QEMU_EFI.fd   ArmVirtQemu code image (2 MiB, runs from GPA 0)
#   test-assets/firmware/QEMU_VARS.fd  blank variable-store template (768 KiB)
#
# Source: Alpine's `aavmf` package (edk2-stable202308 ArmVirtQemu, RELEASE build),
# extracted in a linux/arm64 Docker container. Ternvale does not need the VARS
# template (it formats a missing NVRAM file through EDK2 itself); it is kept for
# comparison with QEMU setups.
#
# Usage:
#   ./scripts/fetch-firmware.sh [test-assets/firmware]
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/firmware}"
alpine_ver="${ALPINE_VER:-3.20}"
# Alpine 3.20 aavmf-0.0.202308-r1. Override EXPECT_SHA256= (empty) for another release.
expect_sha="${EXPECT_SHA256-e05f91e85bf0d5dd9416b7a0d26a45a71c47e73198f761416217d80b00517b88}"

log() {
    printf 'fetch-firmware: %s\n' "$*" >&2
}

if ! docker info >/dev/null 2>&1; then
    log "docker is not reachable; start Docker Desktop and retry"
    exit 1
fi
mkdir -p "${out}"
out=$(cd "${out}" && pwd)

log "extracting aavmf from alpine:${alpine_ver} into ${out}"
docker run --rm --platform linux/arm64 -v "${out}:/out" "alpine:${alpine_ver}" sh -euc '
    apk add --no-cache aavmf >/dev/null
    apk list --installed aavmf
    cp /usr/share/AAVMF/QEMU_EFI.fd /usr/share/AAVMF/QEMU_VARS.fd /out/
'

got=$(shasum -a 256 "${out}/QEMU_EFI.fd" | cut -d' ' -f1)
log "QEMU_EFI.fd sha256 ${got}"
if [[ -n "${expect_sha}" && "${got}" != "${expect_sha}" ]]; then
    log "expected ${expect_sha}; set EXPECT_SHA256= to accept a different build"
    exit 1
fi
ls -l "${out}/QEMU_EFI.fd" "${out}/QEMU_VARS.fd" >&2
