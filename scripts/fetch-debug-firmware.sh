#!/bin/bash
# Fetch the verbose DEBUG EDK2 ArmVirtQemu image the `firmware-debug` boot scenario uses:
#   test-assets/firmware-debug/QEMU_EFI.fd   (3 MiB, DEBUG_GCC, prints DEBUG_INFO lines)
#
# Source: retrage/edk2-nightly, pinned to one commit so the checksum holds.
# QEMU's bundled edk2-aarch64-code.fd is a DEBUG build too but prints errors
# only, so it cannot show AcpiPlatformDxe's INFO lines. This nightly has no
# built-in UEFI shell; the scenario stops at BDS.
#
# Usage:
#   ./scripts/fetch-debug-firmware.sh [test-assets/firmware-debug]
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/firmware-debug}"
commit="${EDK2_NIGHTLY_COMMIT:-bcb92e9466e01f2ef7c390e3ea1346f01ff449b4}"
url="https://raw.githubusercontent.com/retrage/edk2-nightly/${commit}/bin/DEBUGAARCH64_QEMU_EFI.fd"
# Override EXPECT_SHA256= (empty) together with EDK2_NIGHTLY_COMMIT for another build.
expect_sha="${EXPECT_SHA256-0a02772d5f98c5787e68f42ddff6cf9d10f0202440ad24ee26a43da0448464ee}"

log() {
    printf 'fetch-debug-firmware: %s\n' "$*" >&2
}

mkdir -p "${out}"
out=$(cd "${out}" && pwd)
log "downloading ${url}"
curl -fsSL --retry 3 -o "${out}/QEMU_EFI.fd.part" "${url}"
got=$(shasum -a 256 "${out}/QEMU_EFI.fd.part" | cut -d' ' -f1)
log "QEMU_EFI.fd sha256 ${got}"
if [[ -n "${expect_sha}" && "${got}" != "${expect_sha}" ]]; then
    rm -f "${out}/QEMU_EFI.fd.part"
    log "expected ${expect_sha}; set EXPECT_SHA256= to accept a different build"
    exit 1
fi
mv "${out}/QEMU_EFI.fd.part" "${out}/QEMU_EFI.fd"
ls -l "${out}/QEMU_EFI.fd" >&2
