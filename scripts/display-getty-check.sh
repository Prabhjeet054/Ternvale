#!/usr/bin/env bash
# Step 63: keyboard and pointer from the real dev viewer window into a getty on
# the guest's framebuffer console, through `ternvale run --display window`.
#
# The guest boots with `ternvale.getty`, so /init keeps a busybox getty on tty1
# (the framebuffer console; scripts/make-gpu-initramfs.sh). The real Swift
# viewer opens its window and plays a TERNVALE_VIEWER_SELFTEST script
# (tools/ternvale-viewer/selftest.swift): key and mouse NSEvents are sent to
# the window and take the hardware path from there (SurfaceView -> wire ->
# presenter -> keymap -> virtio-input -> Linux VT). Checks, all on serial:
#   1. the typed login name reaches getty ("ternvale getty login name=ternvale");
#   2. a typed shell command with shifted symbols runs ("Typed: Hello, World_42!");
#   3. pointer moves to known guest pixels arrive as ABS_X/ABS_Y events that map
#      back to the same pixels (within 1 px) on the guest's framebuffer.
# A window-only screenshot (screencapture -l) is saved when permitted.
#
# Needs a GUI login session and the assets from scripts/make-gpu-initramfs.sh.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
assets="$root/test-assets/virtio-gpu"
target="${CARGO_TARGET_DIR:-$root/target}"
name="display-getty"
out="$root/target/display-getty/$(date +%Y%m%d-%H%M%S)"
run_dir="/tmp/tvgt-$$"
# Guest pixels the pointer visits; all coordinates distinct so evdev (which
# drops repeated ABS values) passes every move through.
points=("1 1" "640 400" "1279 799" "320 600" "1000 123")
# 3 events per move (ABS_X, ABS_Y, SYN), then a click: 2 reports of 2 events.
ptr_events=$((${#points[@]} * 3 + 4))

log() { printf 'display-getty-check: %s\n' "$*" >&2; }

for f in Image initramfs.cpio; do
    [[ -f "$assets/$f" ]] || { log "missing $assets/$f; run scripts/make-gpu-initramfs.sh"; exit 1; }
done
(cd "$root" && cargo build -p ternvale-cli --quiet)
"$root/scripts/build-viewer.sh"
mkdir -p "$out" "$run_dir"

{
    echo "wait-frame"
    echo "frame-info"
    echo "wait-file $out/login-now"
    echo 'type ternvale\n'
    echo "wait-file $out/command-now"
    echo 'type echo "Typed: Hello, World_$((6*7))!" >/dev/ttyAMA0\n'
    echo "wait-file $out/pointer-now"
    echo "type pointer-check $ptr_events >/dev/ttyAMA0\\n"
    echo "wait-file $out/move-now"
    for p in "${points[@]}"; do
        echo "move $p"
        echo "sleep 50"
    done
    echo "click"
} >"$out/selftest.txt"

cat >"$out/vm.toml" <<EOF
name = "$name"
cpus = 1
ram_mib = 256
kernel = "$assets/Image"
initrd = "$assets/initramfs.cpio"
serial_log = "$out/serial.log"
cmdline = "console=tty0 console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init ternvale.getty"
EOF

export TERNVALE_RUN_DIR="$run_dir" TERNVALE_VIEWER_SELFTEST="$out/selftest.txt" \
    TERNVALE_LOG="info,ternvale::gpu=debug,ternvale::virtio::input=trace"
"$root/scripts/sign-and-run.sh" "$target/debug/ternvale" run "$out/vm.toml" \
    --display window </dev/null >/dev/null 2>"$out/host.log" &
vmm=$!

cleanup() {
    if kill -0 "$vmm" 2>/dev/null; then
        TERNVALE_LOG=warn "$target/debug/ternvale" stop "$name" >/dev/null 2>&1 || kill "$vmm" 2>/dev/null || true
        wait "$vmm" 2>/dev/null || true
    fi
    rm -rf "$run_dir"
}
trap cleanup EXIT

wait_for() { # file pattern seconds
    local i=0
    until grep -qa -- "$2" "$1" 2>/dev/null; do
        kill -0 "$vmm" 2>/dev/null || { log "VMM exited early; see $out/host.log"; exit 1; }
        ((i++ >= $3 * 10)) && { log "timed out waiting for '$2' in $1"; exit 1; }
        sleep 0.1
    done
}
wait_for "$out/host.log" "display viewer hello accepted" 30
wait_for "$out/serial.log" "ternvale getty on tty1" 30
wait_for "$out/host.log" "selftest wait-frame done" 30
size=$(grep -a -m1 "selftest wait-frame done" "$out/host.log" | sed -E 's/.*width=([0-9]+) height=([0-9]+).*/\1 \2/')
read -r width height <<<"$size"
[[ "$width" == 1280 && "$height" == 800 ]] || { log "scanout is ${width}x${height}; the points assume 1280x800"; exit 1; }
sleep 2
touch "$out/login-now"
wait_for "$out/serial.log" "ternvale getty login name=ternvale" 20
log "getty took the login name typed in the window: $(grep -a -m1 'getty login name' "$out/serial.log" | tr -d '\r')"
sleep 0.5
touch "$out/command-now"
wait_for "$out/serial.log" "Typed: Hello, World_42!" 20
log "shell on tty1 ran the command typed in the window: $(grep -a -m1 'Typed: Hello' "$out/serial.log" | tr -d '\r')"

window=$(grep -a -m1 "selftest frame-info" "$out/host.log" | sed -E 's/.*window=([0-9]+).*/\1/')
sleep 0.5
if screencapture -x -o -l "$window" "$out/window.png" 2>/dev/null && [[ -s "$out/window.png" ]]; then
    log "window screenshot $out/window.png"
else
    log "no window screenshot (screencapture -l $window failed; Screen Recording permission?)"
fi

touch "$out/pointer-now"
wait_for "$out/serial.log" "pointer-check ready" 20
touch "$out/move-now"
wait_for "$out/serial.log" "pointer-check done" 70

python3 - "$out/serial.log" "$width" "$height" "${points[@]}" <<'PY'
import re, struct, sys
serial, width, height = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
want = [tuple(int(v) for v in p.split()) for p in sys.argv[4:]]
text = open(serial, "rb").read().decode("latin-1")
body = text.split("pointer-check events", 1)[1].split("pointer-check done", 1)[0]
raw = bytes(int(b, 16) for b in re.findall(r"\b[0-9a-f]{2}\b", body))
if len(raw) % 24:
    sys.exit(f"pointer: {len(raw)} bytes is not a whole number of 24-byte input_events")
x = y = None
seen, buttons = [], []
for off in range(0, len(raw), 24):
    typ, code, value = struct.unpack_from("<HHi", raw, off + 16)
    if typ == 3 and code == 0:
        x = value
    elif typ == 3 and code == 1:
        y = value
    elif typ == 1:
        buttons.append((code, value))
    elif typ == 0 and x is not None and y is not None and (not seen or seen[-1] != (x, y)):
        seen.append((x, y))
ok = len(seen) == len(want) and buttons == [(0x110, 1), (0x110, 0)]
for (gx, gy), (ax, ay) in zip(want, seen):
    px, py = ax * width / 32768, ay * height / 32768
    good = abs(px - (gx + 0.5)) <= 1 and abs(py - (gy + 0.5)) <= 1
    ok &= good
    print(f"display-getty-check: pointer guest pixel ({gx},{gy}) -> ABS ({ax},{ay}) -> "
          f"guest ({px:.2f},{py:.2f}) {'ok' if good else 'MISMATCH'}", file=sys.stderr)
print(f"display-getty-check: pointer moves seen={len(seen)} want={len(want)} buttons={buttons}", file=sys.stderr)
sys.exit(0 if ok else "pointer: guest positions do not track the host 1:1")
PY

TERNVALE_LOG=warn "$target/debug/ternvale" stop "$name" >/dev/null
wait "$vmm" || true

plain="$out/host.plain.log"
sed $'s/\x1b\\[[0-9;]*m//g' "$out/host.log" >"$plain"
kbd=$(grep -c 'virtio-input event injected.*device="keyboard"' "$plain" || true)
tab=$(grep -c 'virtio-input event injected.*device="tablet"' "$plain" || true)
drops=$(grep -c 'virtio-input event dropped' "$plain" || true)
log "TRACE injected: keyboard=$kbd tablet=$tab; WARN drops=$drops"
grep -aE 'selftest (frame-info|typed|move|click)' "$plain" | sed 's/^/  /' >&2
# The host also re-sends the position with the click's mouseDown (3 more events);
# evdev in the guest drops that repeat, so pointer-check never sees it.
[[ "$kbd" -gt 0 && "$tab" -eq $((ptr_events + 3)) && "$drops" -eq 0 ]] || { log "unexpected counts; see $plain"; exit 1; }
log "passed; logs in $out"
