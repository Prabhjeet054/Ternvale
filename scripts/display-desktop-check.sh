#!/usr/bin/env bash
# Step 64: a real desktop workload (Xorg + twm + xterm, docs/DESKTOP.md) on the
# virtio-gpu and virtio-input devices, driven from the real dev viewer window.
#
# Boots test-assets/desktop (scripts/make-desktop-rootfs.sh) with
# `ternvale run --display window`; the guest starts X and reports
# "ternvale desktop ready ... screen=1280x800". The viewer's window self-test
# (TERNVALE_VIEWER_SELFTEST, tools/ternvale-viewer/selftest.swift) then:
#   1. moves the pointer over the xterm (twm focuses the window under the pointer) and
#      types a command with shifted symbols; the xterm's shell runs it, so
#      "Typed in xterm: 42" appears on serial;
#   2. moves the pointer to known guest pixels; after each, `xwhere` (xdotool in the
#      guest, run over the serial console) must report the same X pointer position
#      within 1 px, and over the xterm X must find the xterm under it and focused;
#   3. clicks on the xterm while `xclickwait` (xdotool selectwindow) holds the pointer
#      grab; X must name the clicked window "ternvale-xterm".
# Window-only screenshots (screencapture -l) are saved when permitted: window.png
# after typing, pointer.png with the guest cursor at (640,400). The rootfs is
# cloned per run (cp -c), so the asset stays pristine.
#
# Usage: ./scripts/display-desktop-check.sh [modesetting|fbdev]   (X video driver)
#        DESKTOP_CMDLINE_EXTRA=ternvale.hwcursor ...  (more guest cmdline; see DESKTOP.md)
# Needs a GUI login session.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
assets="$root/test-assets/desktop"
target="${CARGO_TARGET_DIR:-$root/target}"
driver="${1:-modesetting}"
name="display-desktop"
out="$root/target/display-desktop/$(date +%Y%m%d-%H%M%S)-$driver"
run_dir="/tmp/tvdd-$$"
# xterm 80x24 at +100+100 with the 6x13 fixed font: client area about 484x316.
over_xterm="400 250"
points=("1 1" "640 400" "1279 799" "320 600" "1000 123" "$over_xterm")

log() { printf 'display-desktop-check: %s\n' "$*" >&2; }

case "$driver" in modesetting | fbdev) ;; *) log "driver must be modesetting or fbdev"; exit 1 ;; esac
for f in Image initramfs.cpio rootfs.ext4; do
    [[ -f "$assets/$f" ]] || { log "missing $assets/$f; run scripts/make-desktop-rootfs.sh"; exit 1; }
done
"$root/scripts/ternvale-cli-build.sh" --quiet
"$root/scripts/build-viewer.sh"
mkdir -p "$out" "$run_dir"
cp -c "$assets/rootfs.ext4" "$out/rootfs.ext4" 2>/dev/null || cp "$assets/rootfs.ext4" "$out/rootfs.ext4"

{
    echo "wait-frame"
    echo "frame-info"
    echo "wait-file $out/type-now"
    echo "move $over_xterm"
    echo "sleep 300"
    echo 'type echo "Typed in xterm: $((6*7))" >/dev/ttyAMA0\n'
    for i in "${!points[@]}"; do
        echo "wait-file $out/move-$i"
        echo "move ${points[$i]}"
    done
    echo "wait-file $out/click-now"
    echo "move $over_xterm"
    echo "click"
} >"$out/selftest.txt"

cat >"$out/vm.toml" <<EOF
name = "$name"
cpus = 2
ram_mib = 1024
kernel = "$assets/Image"
initrd = "$assets/initramfs.cpio"
serial_log = "$out/serial.log"
cmdline = "console=tty0 console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init ternvale.xdriver=$driver${DESKTOP_CMDLINE_EXTRA:+ $DESKTOP_CMDLINE_EXTRA}"

[[disks]]
path = "$out/rootfs.ext4"
read_only = false
EOF

fifo="$out/stdin"
mkfifo "$fifo"
export TERNVALE_RUN_DIR="$run_dir" TERNVALE_VIEWER_SELFTEST="$out/selftest.txt" \
    TERNVALE_LOG="info,ternvale::gpu=debug,ternvale::virtio::input=trace"
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

wait_for() { # file pattern seconds [count]
    local i=0
    until [[ $(grep -acE -- "$2" "$1" 2>/dev/null) -ge ${4:-1} ]]; do
        kill -0 "$vmm" 2>/dev/null || { log "VMM exited early; see $out/host.log"; exit 1; }
        ((i++ >= $3 * 10)) && { log "timed out waiting for '$2' in $1"; exit 1; }
        sleep 0.1
    done
}
# One byte at a time: the PL011 rx FIFO drops bytes that arrive faster than the
# guest drains it.
serial_send() {
    local cmd="$1" k
    for ((k = 0; k < ${#cmd}; k++)); do
        printf '%s' "${cmd:k:1}" >&3
        sleep 0.005
    done
    printf '\n' >&3
}

wait_for "$out/host.log" "display viewer hello accepted" 30
wait_for "$out/serial.log" "ternvale desktop rootfs ready" 60
wait_for "$out/serial.log" "ternvale desktop (ready|failed)" 90
if grep -aq "ternvale desktop failed" "$out/serial.log"; then
    grep -aE "^(ternvale desktop|xorg:)" "$out/serial.log" | tr -d '\r' | sed 's/^/  /' >&2
    log "the desktop did not start; see $out/serial.log"
    exit 1
fi
ready=$(grep -a -m1 "ternvale desktop ready" "$out/serial.log" | tr -d '\r')
log "guest: ${ready#*ternvale }"
[[ "$ready" == *"screen=1280x800"* ]] || { log "the points assume a 1280x800 X screen"; exit 1; }
sleep 1

touch "$out/type-now"
wait_for "$out/serial.log" "Typed in xterm: 42" 20
log "xterm ran the command typed in the window: $(grep -a -m1 'Typed in xterm' "$out/serial.log" | tr -d '\r')"
sleep 1
window=$(grep -a -m1 "selftest frame-info" "$out/host.log" | sed -E 's/.*window=([0-9]+).*/\1/')
shot() { # file: the viewer window only, never the rest of the screen
    if screencapture -x -o -l "$window" "$out/$1" 2>/dev/null && [[ -s "$out/$1" ]]; then
        log "window screenshot $out/$1"
    else
        log "no window screenshot (screencapture -l $window failed; Screen Recording permission?)"
    fi
}
shot window.png

bad=0
for i in "${!points[@]}"; do
    read -r gx gy <<<"${points[$i]}"
    touch "$out/move-$i"
    wait_for "$out/host.log" "selftest move gx=$gx gy=$gy" 10
    sleep 0.3
    serial_send "xwhere"
    wait_for "$out/serial.log" "^xwhere x=" 10 $((i + 1))
    line=$(grep -a "^xwhere x=" "$out/serial.log" | sed -n "$((i + 1))p" | tr -d '\r')
    x=$(sed -E 's/.*x=([0-9-]+).*/\1/' <<<"$line")
    y=$(sed -E 's/.* y=([0-9-]+).*/\1/' <<<"$line")
    if ((x - gx <= 1 && gx - x <= 1 && y - gy <= 1 && gy - y <= 1)); then verdict=ok; else verdict=MISMATCH; bad=1; fi
    where="window=${line#* window=}"
    if [[ "${points[$i]}" == "$over_xterm" && "$where" != "window=ternvale-xterm focus=ternvale-xterm" ]]; then
        verdict="MISMATCH (expected the xterm under the pointer and focused)"
        bad=1
    fi
    log "pointer host pixel ($gx,$gy) -> X pointer ($x,$y) $where $verdict"
    # xterm hides the pointer while typing; over the bare root window X draws it.
    [[ "${points[$i]}" == "640 400" ]] && shot pointer.png
done
if [[ -s "$out/pointer.png" ]] && sips -s format bmp "$out/pointer.png" --out "$out/pointer.bmp" >/dev/null 2>&1; then
    # The arrow's tip is its hotspot: the top-left-most non-background pixel near
    # (640,400) in the 1280x800 content, which fills the bottom of the window image.
    python3 - "$out/pointer.bmp" 640 400 <<'PY' || bad=1
import struct, sys
data = open(sys.argv[1], "rb").read()
off, w, h, bpp = struct.unpack_from("<I", data, 10)[0], *struct.unpack_from("<ii", data, 18), struct.unpack_from("<H", data, 28)[0]
if bpp not in (24, 32):
    sys.exit(f"pointer.bmp: unsupported {bpp} bpp")
step, row = bpp // 8, ((w * bpp + 31) // 32) * 4
def px(x, y):  # image coordinates, top-left origin
    yy = y if h < 0 else abs(h) - 1 - y
    i = off + yy * row + x * step
    return data[i:i + 3]
scale = w / 1280
top = abs(h) - round(800 * scale)
tx, ty = int(sys.argv[2]), int(sys.argv[3])
bg = px(round((tx - 30) * scale), top + round((ty - 30) * scale))
tip = None
for gy in range(ty - 20, ty + 20):
    for gx in range(tx - 20, tx + 20):
        if px(round((gx + 0.5) * scale), top + round((gy + 0.5) * scale)) != bg:
            tip = (gx, gy)
            break
    if tip:
        break
if tip is None:
    sys.exit("display-desktop-check: no guest cursor drawn near (%d,%d)" % (tx, ty))
ok = abs(tip[0] - tx) <= 2 and abs(tip[1] - ty) <= 2
print(f"display-desktop-check: guest cursor drawn with its tip at {tip} for X pointer ({tx},{ty}) "
      f"{'ok' if ok else 'MISMATCH'}", file=sys.stderr)
sys.exit(0 if ok else 1)
PY
fi
((bad == 0)) || { log "the drawn guest cursor is not where the host put it"; exit 1; }
((bad == 0)) || { log "the X pointer does not track the host 1:1"; exit 1; }

serial_send "xclickwait"
wait_for "$out/serial.log" "xclick ready" 10
sleep 1
touch "$out/click-now"
wait_for "$out/serial.log" "^xclick " 40 2
clicked=$(grep -a "^xclick " "$out/serial.log" | tail -1 | tr -d '\r')
log "click: $clicked"
[[ "$clicked" == "xclick window=ternvale-xterm" ]] || { log "the click did not land on the xterm"; exit 1; }

TERNVALE_LOG=warn "$target/debug/ternvale" stop "$name" >/dev/null
wait "$vmm" || true

plain="$out/host.plain.log"
sed $'s/\x1b\\[[0-9;]*m//g' "$out/host.log" >"$plain"
kbd=$(grep -c 'virtio-input event injected.*device="keyboard"' "$plain" || true)
tab=$(grep -c 'virtio-input event injected.*device="tablet"' "$plain" || true)
drops=$(grep -c 'virtio-input event dropped' "$plain" || true)
log "TRACE injected: keyboard=$kbd tablet=$tab; WARN drops=$drops"
grep -aE 'display presenter stopped|WARN|ERROR' "$plain" | grep -v 'unmapped mmio access' | cut -c1-260 | sed 's/^/  /' >&2 || true
[[ "$kbd" -gt 0 && "$tab" -gt 0 && "$drops" -eq 0 ]] || { log "unexpected counts; see $plain"; exit 1; }
log "passed; logs in $out"
