#!/usr/bin/env bash
# Step 44 latency baseline: boot the virtio-gpu test kernel to a framebuffer
# console with `ternvale run --display window`, keep the console busy, and
# measure RESOURCE_FLUSH -> on-screen present for SECONDS_RUN seconds.
#
# Needs a logged-in GUI session (the dev viewer opens a window) and the assets
# from scripts/make-gpu-initramfs.sh. Prints count / avg / p50 / p95 / max of
# flush_to_present_us over the measured window and writes them to
# <out>/summary.txt; the presenter's own `display present window` (every 10 s)
# and `display presenter stopped` lines are in <out>/host.log.
#
# Env: FPS (30), SECONDS_RUN (30), WORKLOAD (scroll | idle; scroll prints
# 50 lines/s to the console, idle leaves only the fbcon cursor blink).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
fps="${FPS:-30}"
seconds="${SECONDS_RUN:-30}"
workload="${WORKLOAD:-scroll}"
assets="$root/test-assets/virtio-gpu"
name="display-latency"
out="$root/target/display-latency/$(date +%Y%m%d-%H%M%S)-fps${fps}-${workload}"
run_dir="/tmp/tvlat-$$"

log() { printf 'display-latency: %s\n' "$*" >&2; }

for f in Image initramfs.cpio; do
    [[ -f "$assets/$f" ]] || { log "missing $assets/$f; run scripts/make-gpu-initramfs.sh"; exit 1; }
done
case "$workload" in scroll | idle) ;; *) log "WORKLOAD must be scroll or idle"; exit 2 ;; esac

log "building ternvale and the viewer"
(cd "$root" && cargo build -p ternvale-cli --quiet)
"$root/scripts/build-viewer.sh"

mkdir -p "$out" "$run_dir"
cat >"$out/vm.toml" <<EOF
name = "$name"
cpus = 1
ram_mib = 256
kernel = "$assets/Image"
initrd = "$assets/initramfs.cpio"
serial_log = "$out/serial.log"
cmdline = "console=tty0 console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init"
EOF

fifo="$out/stdin"
mkfifo "$fifo"
export TERNVALE_RUN_DIR="$run_dir" TERNVALE_LOG="info,ternvale::gpu=debug"
"$root/scripts/sign-and-run.sh" "$root/target/debug/ternvale" run "$out/vm.toml" \
    --display window --fps "$fps" <"$fifo" >/dev/null 2>"$out/host.log" &
vmm=$!
exec 3>"$fifo"

cleanup() {
    exec 3>&- || true
    if kill -0 "$vmm" 2>/dev/null; then
        TERNVALE_LOG=warn "$root/target/debug/ternvale" stop "$name" >/dev/null 2>&1 || kill "$vmm" 2>/dev/null || true
        wait "$vmm" 2>/dev/null || true
    fi
    rm -rf "$run_dir"
}
trap cleanup EXIT

wait_for() { # file pattern seconds
    local i=0
    until grep -q "$2" "$1" 2>/dev/null; do
        kill -0 "$vmm" 2>/dev/null || { log "VMM exited early; see $out/host.log"; exit 1; }
        ((i++ >= $3 * 10)) && { log "timed out waiting for '$2' in $1"; exit 1; }
        sleep 0.1
    done
}
wait_for "$out/host.log" "display viewer hello accepted" 20
wait_for "$out/serial.log" "ternvale gpu ready fb0=virtio_gpudrmfb" 30
sleep 1
if [[ "$workload" == scroll ]]; then
    # One byte at a time: the PL011 rx FIFO drops bytes that arrive faster
    # than the guest drains it.
    cmd='i=0; while :; do echo "ternvale latency baseline line $i $(date +%T)"; i=$((i+1)); sleep 0.02; done >/dev/tty1 &'
    for ((k = 0; k < ${#cmd}; k++)); do
        printf '%s' "${cmd:k:1}" >&3
        sleep 0.005
    done
    printf '\n' >&3
    wait_for "$out/serial.log" 'done >/dev/tty1 &' 10
fi
start_line=$(($(wc -l <"$out/host.log") + 1))
log "measuring for ${seconds}s (fps cap $fps, workload $workload)"
sleep "$seconds"
end_line=$(wc -l <"$out/host.log")
TERNVALE_LOG=warn "$root/target/debug/ternvale" stop "$name" >/dev/null
wait "$vmm" || true

sed $'s/\x1b\\[[0-9;]*m//g' "$out/host.log" | sed -n "${start_line},${end_line}p" >"$out/window.log"
summary=$(awk -v secs="$seconds" -v fps="$fps" -v wl="$workload" '
    /frame presented/ {
        for (i = 1; i <= NF; i++) {
            if ($i ~ /^flush_to_present_us=/) { split($i, kv, "="); lat[n++] = kv[2] + 0 }
            if ($i ~ /^send_to_present_us=/) { split($i, kv, "="); send += kv[2]; sends++ }
        }
    }
    /frames dropped/ {
        for (i = 1; i <= NF; i++) if ($i ~ /^count=/) { split($i, kv, "="); dropped += kv[2] }
    }
    END {
        if (n == 0) { print "no frames presented in the window"; exit 1 }
        for (i = 1; i < n; i++) { v = lat[i]; j = i - 1; while (j >= 0 && lat[j] > v) { lat[j + 1] = lat[j]; j-- } lat[j + 1] = v }
        for (i = 0; i < n; i++) sum += lat[i]
        printf "window=%ss fps_cap=%s workload=%s frames=%d presented_fps=%.1f dropped=%d\n", secs, fps, wl, n, n / secs, dropped
        printf "flush_to_present_us avg=%d p50=%d p95=%d max=%d min=%d\n", sum / n, lat[int(n * 0.5)], lat[int(n * 0.95)], lat[n - 1], lat[0]
        printf "send_to_present_us avg=%d\n", send / sends
    }' "$out/window.log")
printf '%s\n' "$summary" | tee "$out/summary.txt"
grep -E "display presenter stopped" "$out/host.log" | sed $'s/\x1b\\[[0-9;]*m//g' >>"$out/summary.txt" || true
log "logs in $out"
