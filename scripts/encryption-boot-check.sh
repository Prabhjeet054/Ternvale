#!/bin/bash
# Encrypted vs plaintext marker on COW deltas (hypervisor).
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
assets="${root}/test-assets/snapshot-boot"
log() { printf 'encryption-boot-check: %s\n' "$*" >&2; }

if [[ ! -f "${assets}/Image" || ! -f "${assets}/initramfs.cpio" ]]; then
  log "building guest assets"
  "${script_dir}/make-snapshot-initramfs.sh" "${assets}"
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${root}/target}"
TERNVALE_BOOT_SCENARIO=encryption "${script_dir}/boot-test.sh"
