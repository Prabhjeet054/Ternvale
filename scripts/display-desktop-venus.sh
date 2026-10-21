#!/usr/bin/env bash
# Boot the desktop guest with `ternvale-cli --features gpu-3d`, run Mesa's Venus probe
# (ternvale-3d-probe), and require non-software Vulkan/OpenGL renderers.
#
# Prereqs:
#   - GUI session (dev viewer + screencapture)
#   - test-assets/desktop with VENUS=1: ./scripts/make-desktop-rootfs.sh
#     (or let this script overlay Mesa packages into a per-run rootfs clone)
#   - Host virglrenderer with Venus for real acceleration (see docs/GPU_ACCELERATION.md);
#     without it the guest typically reports llvmpipe and this script exits non-zero.
#
# Usage: ./scripts/display-desktop-venus.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
assets="$root/test-assets/desktop"
target="${CARGO_TARGET_DIR:-$root/target}"
name="display-desktop-venus"
out="$root/target/display-desktop-venus/$(date +%Y%m%d-%H%M%S)"
run_dir="/tmp/tvvenus-$$"

log() { printf 'display-desktop-venus: %s\n' "$*" >&2; }

for f in Image initramfs.cpio rootfs.ext4; do
    [[ -f "$assets/$f" ]] || { log "missing $assets/$f; run scripts/make-desktop-rootfs.sh"; exit 1; }
done

mkdir -p "$out" "$run_dir"
cp -c "$assets/rootfs.ext4" "$out/rootfs.ext4" 2>/dev/null || cp "$assets/rootfs.ext4" "$out/rootfs.ext4"

guest="$root/scripts/desktop-guest"
if ! docker run --rm --privileged --platform linux/arm64 \
    -v "$out/rootfs.ext4:/disk.ext4:rw" \
    -v "$guest:/guest:ro" \
    alpine:3.20 sh -euc '
mount -o loop /disk.ext4 /mnt
apk add --no-cache e2fsprogs >/dev/null
install -m 755 /guest/ternvale-3d-probe /guest/ternvale-desktop /mnt/usr/local/bin/
if ! command -v vulkaninfo >/dev/null 2>&1; then
  apk --root /mnt --no-cache add mesa mesa-gl mesa-egl mesa-dri-gallium mesa-gles \
      vulkan-loader vulkan-tools mesa-demos >/dev/null
fi
umount /mnt
'; then
    log "could not ensure Mesa/Vulkan packages in rootfs (docker failed)"
    exit 1
fi

export TERNVALE_CLI_FEATURES=gpu-3d
"$root/scripts/ternvale-cli-build.sh" --quiet
"$root/scripts/build-viewer.sh"

cat >"$out/vm.toml" <<EOF
name = "$name"
cpus = 2
ram_mib = 1024
kernel = "$assets/Image"
initrd = "$assets/initramfs.cpio"
serial_log = "$out/serial.log"
cmdline = "console=tty0 console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init ternvale.venus"

[[disks]]
path = "$out/rootfs.ext4"
read_only = false
EOF

fifo="$out/stdin"
mkfifo "$fifo"
export TERNVALE_RUN_DIR="$run_dir" \
    TERNVALE_LOG="info,ternvale::gpu=debug"
"$root/scripts/sign-and-run.sh" "$target/debug/ternvale" run "$out/vm.toml" \
    --display window <"$fifo" >/dev/null 2>"$out/host.log" &
vmm=$!
exec 3>"$fifo"

cleanup() {
    exec 3>&- || true
    if kill -0 "$vmm" 2>/dev/null; then
        TERNVALE_LOG=warn "$target/debug/ternvale" stop "$name" >/dev/null 2>&1 || kill "$vmm" 2>/dev/null || true
        wait "$vmm" 2>/dev/null || true
    fi
    rm -rf "$run_dir"
}
trap cleanup EXIT

wait_for() {
    local file="$1" pattern="$2" seconds="$3"
    local i=0
    until grep -aqE "$pattern" "$file" 2>/dev/null; do
        kill -0 "$vmm" 2>/dev/null || { log "VMM exited; see $out/host.log"; exit 1; }
        ((i++ >= seconds * 10)) && { log "timeout waiting for $pattern in $file"; exit 1; }
        sleep 0.1
    done
}

wait_for "$out/host.log" "display viewer hello accepted" 45
wait_for "$out/serial.log" "ternvale desktop ready" 120
wait_for "$out/serial.log" "ternvale 3d probe: finished" 60

grep -a "ternvale 3d probe:" "$out/serial.log" | tr -d '\r' | sed 's/^/  /' >&2

bad=0
if grep -aq "vulkan_software_fallback=yes" "$out/serial.log"; then
    log "Vulkan fell back to software (llvmpipe/lavapipe)"
    bad=1
fi
if grep -aq "opengl_software_fallback=yes" "$out/serial.log"; then
    log "OpenGL fell back to software"
    bad=1
fi
if ! grep -aq "glxgears_.*FPS" "$out/serial.log"; then
    log "glxgears did not report FPS (missing or failed)"
    bad=1
fi
if grep -aq "virtio-gpu 3d command failed" "$out/host.log"; then
    log "host rejected Venus SUBMIT_3D commands (see $out/host.log)"
    bad=1
fi

window=$(grep -a -m1 "selftest frame-info" "$out/host.log" 2>/dev/null | sed -E 's/.*window=([0-9]+).*/\1/' || true)
if [[ -n "$window" ]] && screencapture -x -o -l "$window" "$out/venus-desktop.png" 2>/dev/null; then
    log "screenshot $out/venus-desktop.png"
fi

if ((bad != 0)); then
    log "Venus acceleration check FAILED (install virglrenderer + link host renderer, or see Step 68)"
    exit 1
fi
log "Venus acceleration check passed; artifacts in $out"
