#!/bin/bash
# Full-VM snapshot rewind E2E: guest counter must rewind after restore (hypervisor required).
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
assets="${root}/test-assets/vm-snapshot"

log() {
    printf 'vm-snapshot-restore-check: %s\n' "$*" >&2
}

if [[ ! -f "${assets}/Image" || ! -f "${assets}/initramfs.cpio" ]]; then
    log "building guest assets"
    "${script_dir}/make-vm-snapshot-initramfs.sh" "${assets}"
fi

ram_mib="${TERNVALE_BOOT_RAM_MIB:-256}"
export TERNVALE_BOOT_RAM_MIB="${ram_mib}"
export TERNVALE_BOOT_SCENARIO=vm-snapshot
log "running boot harness (ram_mib=${ram_mib})"
TERNVALE_BOOT_SCENARIO=vm-snapshot TERNVALE_BOOT_RAM_MIB="${ram_mib}" \
    "${script_dir}/boot-test.sh"
