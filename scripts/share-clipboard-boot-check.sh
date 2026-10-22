#!/bin/bash
# Run the share-clipboard boot harness (needs hypervisor entitlement + devices initramfs).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
"${root}/scripts/make-devices-initramfs.sh"
export TERNVALE_BOOT_SCENARIO=share-clipboard
exec "${root}/scripts/boot-test.sh"
