#!/bin/bash
# Live virtio-fs checks (needs hypervisor entitlement + rebuilt devices initramfs).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
"${root}/scripts/make-devices-initramfs.sh"
export TERNVALE_BOOT_SCENARIO=virtiofs
exec "${root}/scripts/boot-test.sh"
