#!/usr/bin/env bash
# Step 107 test: feature-off, GPU-lost analog, decoder soak, optional Venus guest.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/target/gpu3d-live/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out"
log() { printf 'gpu3d-live: %s\n' "$*" >&2; }

cd "$root"
summary="$out/summary.txt"
: >"$summary"
note() { printf '%s\n' "$*" | tee -a "$summary" >&2; }

note "feature_off: cargo test gpu3d_runtime_off + config default"
cargo test -p ternvale-gpu gpu3d_runtime_off_rejects_ctx_create -- --test-threads=1
cargo test -p ternvale-gpu virtio_mmio_identity_config_and_a_frame_end_to_end -- --test-threads=1
cargo test -p ternvale-config gpu3d_defaults -- --test-threads=1
note "feature_off=ok"

note "gpu_lost: renderer death analog (ERROR, no panic)"
cargo test -p ternvale-gpu3d-host renderer_death_marks_gpu_lost_at_error_without_panic -- --test-threads=1 --nocapture
note "gpu_lost=ok"

note "decoder_burst"
cargo test -p ternvale-gpu3d-host decoder_burst_does_not_panic -- --test-threads=1
note "decoder_burst=ok"

fuzz_secs="${TERNVALE_GPU3D_FUZZ_SECS:-600}"
if [[ "$fuzz_secs" == "0" ]]; then
    note "fuzz_10m=skip (TERNVALE_GPU3D_FUZZ_SECS=0)"
else
    note "fuzz_10m: decoder soak ${fuzz_secs}s"
    TERNVALE_GPU3D_FUZZ_SECS="$fuzz_secs" cargo test -p ternvale-gpu3d-host \
        fuzz_decoder_ten_minutes -- --ignored --test-threads=1 --nocapture \
        | tee "$out/fuzz.log"
    note "fuzz_10m=ok secs=$fuzz_secs"
fi

venus="$root/scripts/display-desktop-venus.sh"
assets="$root/test-assets/desktop"
if [[ -x "$venus" && -f "$assets/Image" && -f "$assets/rootfs.ext4" ]]; then
    note "venus_guest: $venus"
    if "$venus" >"$out/venus.log" 2>&1; then
        note "venus_guest=ok (vulkaninfo/vkcube path via ternvale-3d-probe)"
        grep -E 'vulkan_device=|vkcube=|glxgears_.*FPS|software_fallback=' "$out/venus.log" \
            | tee -a "$summary" || true
    else
        note "venus_guest=fail (see $out/venus.log)"
        tail -n 40 "$out/venus.log" >&2 || true
        note "vkcube=skip_or_fail"
        note "fps_vs_llvmpipe=not_recorded"
    fi
else
    note "venus_guest=skip_no_desktop_assets"
    note "vulkaninfo=skip"
    note "vkcube=skip"
    note "fps_vs_llvmpipe=skip"
fi

host_gfx="${TERNVALE_HOST_GFX:-$HOME/Library/Application Support/Ternvale/host-gfx}"
if [[ -f "$host_gfx/lib/libvirglrenderer.1.dylib" \
    || -f /opt/homebrew/lib/libvirglrenderer.1.dylib \
    || -f /usr/local/lib/libvirglrenderer.1.dylib ]]; then
    note "libvirglrenderer=present"
else
    note "libvirglrenderer=missing"
fi

note "artifacts $out"
cat "$summary"
