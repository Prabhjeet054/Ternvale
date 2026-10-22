#!/bin/bash
# Linked-clone E2E: `ternvale clone` x3, concurrent boots, base digest unchanged, delete two clones.
# Requires hypervisor entitlement (same as boot-test.sh).
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
virtio="${root}/test-assets/virtio-root"
clone_assets="${root}/test-assets/clone-boot"
log() { printf 'clone-boot-check: %s\n' "$*" >&2; }

if [[ ! -f "${virtio}/rootfs.ext4" ]]; then
    log "building virtio-root assets"
    "${script_dir}/make-rootfs.sh"
fi
if [[ ! -f "${clone_assets}/initramfs.cpio" ]]; then
    log "building clone-boot initramfs"
    "${script_dir}/make-clone-initramfs.sh" "${clone_assets}"
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${root}/target}"
work="${CARGO_TARGET_DIR}/clone-e2e-$$"
vms="${work}/vms"
refs="${work}/refs"
mkdir -p "${vms}/source/disks"
trap 'rm -rf "${work}"' EXIT

cp -c "${virtio}/rootfs.ext4" "${vms}/source/disks/disk0.img" 2>/dev/null \
    || cp "${virtio}/rootfs.ext4" "${vms}/source/disks/disk0.img"

kernel="${virtio}/Image"
serial="${vms}/source/serial.log"
cat >"${vms}/source/config.toml" <<EOF
name = "source"
cpus = 1
ram_mib = 256
kernel = "${kernel}"
serial_log = "${serial}"
cmdline = "console=ttyAMA0"

[[disks]]
path = "${vms}/source/disks/disk0.img"
read_only = false
EOF

log "building ternvale CLI"
cargo build -p ternvale-cli --quiet
bin="${CARGO_TARGET_DIR}/debug/ternvale"
export TERNVALE_VMS_DIR="${vms}"
export TERNVALE_REFS_DIR="${refs}"
export TERNVALE_CLONE_USE_CLI=1
export TERNVALE_BIN="${bin}"
export TERNVALE_CLONE_VMS="clone-a,clone-b,clone-c"

for id in a b c; do
    log "ternvale clone source -> clone-${id}"
    "${bin}" clone source "clone-${id}"
done

base_digest_before=$(shasum -a 256 "${vms}/source/disks/disk0.img" | awk '{print $1}')
log "base sha256 before boots: ${base_digest_before}"

TERNVALE_BOOT_SCENARIO=clone "${script_dir}/boot-test.sh"
status=$?

base_digest_after=$(shasum -a 256 "${vms}/source/disks/disk0.img" | awk '{print $1}')
if [[ "${base_digest_before}" != "${base_digest_after}" ]]; then
    log "base image changed on disk (before ${base_digest_before}, after ${base_digest_after})"
    exit 1
fi
log "base sha256 unchanged after harness"
exit "${status}"
