#!/bin/bash
# End-to-end virtio-sound check: build guest assets, boot Linux, play a 440 Hz sine.
# Listen on the host during the run; logs must show negotiated PCM format and few underruns.
#
# Usage:
#   ./scripts/audio-boot-check.sh
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
assets="${root}/test-assets/virtio-audio"

log() {
    printf 'audio-boot-check: %s\n' "$*" >&2
}

if [[ ! -f "${assets}/Image" || ! -f "${assets}/initramfs.cpio" ]]; then
    log "building guest assets"
    "${script_dir}/make-audio-initramfs.sh" "${assets}"
fi

log "booting guest (listen for a short 440 Hz tone on the host speakers)"
log "host logs: target/boot-logs/<pid>/"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${root}/target}"
TERNVALE_BOOT_SCENARIO=audio \
    TERNVALE_BOOT_LOG="${TERNVALE_BOOT_LOG:-info,ternvale::virtio::sound=debug}" \
    "${script_dir}/boot-test.sh"
