#!/usr/bin/env bash
# Step 65: the desktop guest (docs/DESKTOP.md) under the real dev viewer, used the
# way a person would, then stressed. Phases, all through the viewer window's
# self-test (TERNVALE_VIEWER_SELFTEST):
#   1. render   - twm and the startup xterm are on screen (window-only screenshot,
#                 desktop.png; X reports the screen and both windows mapped).
#   2. menu     - press on the root window, drag onto the twm menu's "XTerm" entry,
#                 release: twm starts `menu-xterm` (xdotool finds it mapped); then a
#                 command typed into it runs ("Typed in menu xterm: 42").
#   3. keys     - burst: a long random line typed with keygap 0 (as fast as the
#                 viewer can post); an evdev tap in the guest counts what the
#                 kernel received and any SYN_DROPPED (reported; see 3a below).
#                 fast: another line at KEYGAP us per key event (default 1000,
#                 about 500 characters/s) written to a file; its md5 must match.
#   4. mouse    - stress-move: thousands of moves (about 1 kHz) along a figure-eight,
#                 ending at (640,400), which X must report exactly.
#   5. clicks   - a burst of clicks over the startup xterm, then a command typed
#                 there must still run.
# Then the host log is checked for dropped input events (WARN), frame stalls
# (presenter WARNs, long gaps between presented frames while the pointer moves),
# and the presented FPS during the mouse phase is measured.
#
# Usage: FPS=30 KEYS=1500 KEYGAP=1000 MOVES=10000 ./scripts/display-desktop-stress.sh
#        (EXTRA_LOG=ternvale::lock=trace adds TERNVALE_LOG directives; STRESS_LOG
#        replaces the default ones, which the FPS/stall analysis needs)
# Needs a GUI login session and test-assets/desktop (scripts/make-desktop-rootfs.sh).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
assets="$root/test-assets/desktop"
target="${CARGO_TARGET_DIR:-$root/target}"
fps="${FPS:-30}"
keys="${KEYS:-1500}"
moves="${MOVES:-10000}"
keygap="${KEYGAP:-1000}"
name="desktop-stress"
out="$root/target/desktop-stress/$(date +%Y%m%d-%H%M%S)-fps$fps"
run_dir="/tmp/tvst-$$"
menu_press="900 150"
# twm pops the root menu with its title under the pointer; "XTerm" is the next entry.
menu_xterm_entry="900 166"
over_menu_xterm="880 540"
over_xterm="400 250"

log() { printf 'desktop-stress: %s\n' "$*" >&2; }

for f in Image initramfs.cpio rootfs.ext4; do
    [[ -f "$assets/$f" ]] || { log "missing $assets/$f; run scripts/make-desktop-rootfs.sh"; exit 1; }
done
(cd "$root" && cargo build -p ternvale-cli --quiet)
"$root/scripts/build-viewer.sh"
mkdir -p "$out" "$run_dir"
cp -c "$assets/rootfs.ext4" "$out/rootfs.ext4" 2>/dev/null || cp "$assets/rootfs.ext4" "$out/rootfs.ext4"
random_line() { python3 -c 'import random, string, sys; print("".join(random.choice(string.ascii_letters + string.digits) for _ in range(int(sys.argv[1]))))' "$keys"; }
payload=$(random_line)
burst=$(random_line)
want_md5=$(printf '%s\n' "$payload" | md5 -q)

{
    echo "wait-frame"
    echo "frame-info"
    echo "wait-file $out/menu-now"
    echo "move $menu_press"
    echo "press"
    echo "sleep 400"
    echo "wait-file $out/menu-shot-done"
    echo "move $menu_xterm_entry"
    echo "sleep 200"
    echo "release"
    echo "wait-file $out/type-menu-now"
    echo "move $over_menu_xterm"
    echo "sleep 300"
    echo 'type echo "Typed in menu xterm: $((6*7))" >/dev/ttyAMA0\n'
    echo "wait-file $out/burst-now"
    echo "keygap 0"
    echo "type echo $burst >/tmp/burst\\n"
    echo "keygap 8000"
    echo "wait-file $out/keys-now"
    echo "move $over_xterm"
    echo "sleep 300"
    echo "keygap $keygap"
    echo "type echo $payload >/tmp/keys\\n"
    echo "keygap 7999"
    echo "wait-file $out/mouse-now"
    echo "stress-move $moves 1000 640 400"
    echo "wait-file $out/clicks-now"
    echo "move $over_xterm"
    echo "click-burst 200 2000"
    echo "sleep 500"
    echo 'type echo "after stress: $((6*7))" >/dev/ttyAMA0\n'
} >"$out/selftest.txt"

cat >"$out/vm.toml" <<EOF
name = "$name"
cpus = 2
ram_mib = 1024
kernel = "$assets/Image"
initrd = "$assets/initramfs.cpio"
serial_log = "$out/serial.log"
cmdline = "console=tty0 console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init"

[[disks]]
path = "$out/rootfs.ext4"
read_only = false
EOF

fifo="$out/stdin"
mkfifo "$fifo"
export TERNVALE_RUN_DIR="$run_dir" TERNVALE_VIEWER_SELFTEST="$out/selftest.txt" \
    TERNVALE_LOG="${STRESS_LOG:-info,ternvale::gpu=debug,ternvale::virtio::input=trace}${EXTRA_LOG:+,$EXTRA_LOG}"
"$root/scripts/sign-and-run.sh" "$target/debug/ternvale" run "$out/vm.toml" \
    --display window --fps "$fps" <"$fifo" >/dev/null 2>"$out/host.log" &
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
serial_send() { # one byte at a time: the PL011 rx FIFO is 16 bytes
    local cmd="$1" k
    for ((k = 0; k < ${#cmd}; k++)); do
        printf '%s' "${cmd:k:1}" >&3
        sleep 0.005
    done
    printf '\n' >&3
}
serial_line() { grep -a "$1" "$out/serial.log" | tail -1 | tr -d '\r'; }

# 1. render
wait_for "$out/host.log" "display viewer hello accepted" 30
wait_for "$out/serial.log" "ternvale desktop (ready|failed)" 90
grep -aq "ternvale desktop failed" "$out/serial.log" && { log "the desktop did not start; see $out/serial.log"; exit 1; }
log "guest: $(serial_line 'ternvale desktop ready' | sed 's/.*ternvale //')"
window=$(grep -a -m1 "selftest frame-info" "$out/host.log" | sed -E 's/.*window=([0-9]+).*/\1/')
shot() { # file: the viewer window only, never the rest of the screen
    if screencapture -x -o -l "$window" "$out/$1" 2>/dev/null && [[ -s "$out/$1" ]]; then
        log "window screenshot $out/$1"
    else
        log "no window screenshot $1 (Screen Recording permission?)"
    fi
}
sleep 1
serial_send "export DISPLAY=:0"
serial_send "xdotool search --onlyvisible --name . getwindowname %@ | sort | tr '\\n' ' ' | sed 's/^/mapped: /'; echo"
wait_for "$out/serial.log" "^mapped: " 10
log "render: $(serial_line '^mapped: ') (twm frames + the startup xterm)"
grep -a "^mapped: " "$out/serial.log" | grep -q "ternvale-xterm" || { log "the startup xterm is not mapped"; exit 1; }
shot desktop.png

# 2. menu: open a terminal with the mouse, then type into it
touch "$out/menu-now"
wait_for "$out/host.log" "selftest press" 10
sleep 0.6
shot menu.png
touch "$out/menu-shot-done"
wait_for "$out/host.log" "selftest release" 10
serial_send "xdotool search --sync --onlyvisible --name menu-xterm getwindowgeometry | tr '\\n' ' ' | sed 's/^/menu-xterm: /'; echo"
wait_for "$out/serial.log" "^menu-xterm: Window" 20
log "menu: $(serial_line '^menu-xterm: ')"
sleep 0.5
touch "$out/type-menu-now"
wait_for "$out/serial.log" "Typed in menu xterm: 42" 20
log "menu: the terminal opened by mouse click ran a typed command: $(serial_line 'Typed in menu xterm')"
sleep 0.5
shot menu-xterm.png

# 3a. key burst (keygap 0, into menu-xterm). Faster than Linux evdev clients are
# sized for: a reader's buffer (64 events for a keyboard) overflows and the
# kernel sends it SYN_DROPPED. A second reader taps the evdev node (24-byte arm64
# input_event records) to count what the guest kernel received. Only a loss with
# no SYN_DROPPED (before evdev, i.e. in the VMM or virtio path) fails.
# evdev rejects reads shorter than one record, hence dd with a large block.
tap_start() { serial_send "dd if=/dev/input/event0 of=/tmp/ev0.bin bs=4800 2>/dev/null &"; sleep 0.5; }
tap_stop() { # label -> sets tap_presses, tap_dropped
    serial_send "kill \$!; hexdump -v -e '24/1 \"%02x\" \"\\n\"' /tmp/ev0.bin | awk '{t=substr(\$0,33,4); c=substr(\$0,37,4); v=substr(\$0,41,8); if (t==\"0100\" && v==\"01000000\") p++; if (t==\"0000\" && c==\"0300\") d++} END {print \"evtap-$1: presses=\" p+0 \" syn_dropped=\" d+0}'"
    wait_for "$out/serial.log" "^evtap-$1: " 10
    local tap
    tap=$(serial_line "^evtap-$1: ")
    tap_presses=$(sed -E 's/.*presses=([0-9]+).*/\1/' <<<"$tap")
    tap_dropped=$(sed -E 's/.*syn_dropped=([0-9]+).*/\1/' <<<"$tap")
}
presses_for() { # the line the viewer types; shift is held across shifted runs
    python3 -c 'import sys, string
t = sys.argv[1] + "\n"
up = [c in string.ascii_uppercase or c == ">" for c in t]
print(len(t) + sum(1 for i, u in enumerate(up) if u and (i == 0 or not up[i - 1])))' "$1"
}
tap_start
burst_from=$(wc -l <"$out/host.log")
touch "$out/burst-now"
wait_for "$out/host.log" "selftest keygap us=8000" 60
sleep 2
burst_to=$(wc -l <"$out/host.log")
tap_stop burst
want_presses=$(presses_for "echo $burst >/tmp/burst")
serial_send "md5sum /tmp/burst 2>&1 | sed 's/^/burst-md5: /'"
wait_for "$out/serial.log" "^burst-md5: " 10
burst_md5=$(serial_line '^burst-md5: ' | sed 's/burst-md5: //' | cut -c1-32)
burst_ok=no
[[ "$burst_md5" == "$(printf '%s\n' "$burst" | md5 -q)" ]] && burst_ok=yes
log "burst: ${#burst} characters with keygap 0: host injected presses=$want_presses; guest evdev tap presses=$tap_presses syn_dropped=$tap_dropped; line intact in xterm: $burst_ok"
if ((tap_presses < want_presses && tap_dropped == 0)); then
    log "burst: key presses lost before evdev (no SYN_DROPPED)"
    exit 1
fi

# 3b. fast typing (keygap $keygap us, into the startup xterm): must arrive exactly.
tap_start
touch "$out/keys-now"
wait_for "$out/host.log" "selftest keygap us=7999" 60
sleep 2
tap_stop keys
log "keys: host injected presses=$(presses_for "echo $payload >/tmp/keys"); guest evdev tap presses=$tap_presses syn_dropped=$tap_dropped"
serial_send "md5sum /tmp/keys"
wait_for "$out/serial.log" "^[0-9a-f]{32}  /tmp/keys" 30
got_md5=$(serial_line '^[0-9a-f]\{32\}  /tmp/keys' | cut -c1-32)
key_result="keys: ${#payload} random characters typed with keygap ${keygap} us; guest md5 $got_md5, host md5 $want_md5"
if [[ "$got_md5" != "$want_md5" ]]; then
    serial_send "echo keys-begin; cat /tmp/keys; echo keys-end"
    wait_for "$out/serial.log" "^keys-end" 30
    printf '%s\n' "$payload" >"$out/keys.want"
    sed -n '/^keys-begin/,/^keys-end/p' "$out/serial.log" | tr -d '\r' | sed '1d;$d' >"$out/keys.got"
    python3 - "$out/keys.want" "$out/keys.got" <<'DIFF' >&2
import sys
want, got = (open(p).read().rstrip("\n") for p in sys.argv[1:3])
print(f"desktop-stress: guest line {len(got)} chars, host {len(want)}")
for i, (a, b) in enumerate(zip(want, got)):
    if a != b:
        print(f"desktop-stress: first difference at {i}: host ...{want[max(0, i - 8):i + 12]!r} guest ...{got[max(0, i - 8):i + 12]!r}")
        break
DIFF
    serial_send "grep -ciE 'drop' /var/log/Xorg.0.log | sed 's/^/xorg-drop-lines: /'; grep -iE 'drop' /var/log/Xorg.0.log | tail -5 | sed 's/^/xorg: /'"
    wait_for "$out/serial.log" "^xorg-drop-lines: " 10
    grep -aE "^(xorg-drop-lines|xorg): " "$out/serial.log" | tr -d '\r' | sed 's/^/desktop-stress: guest /' >&2
    log "$key_result MISMATCH"
    exit 1
fi
log "$key_result ok"

# 4. mouse stress
touch "$out/mouse-now"
wait_for "$out/host.log" "selftest move gx=640 gy=400" 120
sleep 0.5
serial_send "xwhere"
wait_for "$out/serial.log" "^xwhere x=" 10
where=$(serial_line '^xwhere x=')
log "mouse: $(grep -a 'selftest stress-move done' "$out/host.log" | sed 's/.*selftest //'); $where"
[[ "$where" == "xwhere x=640 y=400 "* ]] || { log "the X pointer did not end at (640,400)"; exit 1; }

# 5. click burst, then input must still work
touch "$out/clicks-now"
wait_for "$out/serial.log" "after stress: 42" 30
log "clicks: $(grep -a 'selftest click-burst done' "$out/host.log" | sed 's/.*selftest //'); then: $(serial_line 'after stress')"
shot after.png

TERNVALE_LOG=warn "$target/debug/ternvale" stop "$name" >/dev/null
wait "$vmm" || true

plain="$out/host.plain.log"
sed $'s/\x1b\\[[0-9;]*m//g' "$out/host.log" >"$plain"
python3 - "$plain" "$fps" "$burst_from" "$burst_to" <<'PY'
import re, sys
from datetime import datetime
path, cap, burst_from, burst_to = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
def ts(line):
    return datetime.strptime(line[:26], "%Y-%m-%dT%H:%M:%S.%f").timestamp()
lines = open(path, errors="replace").read().splitlines()
# Drops during the keygap-0 burst are the bounded backlog working as designed.
burst_drops = [l for l in lines[burst_from:burst_to] if "virtio-input event dropped" in l]
lines = lines[:burst_from] + lines[burst_to:]
stamped = [l for l in lines if re.match(r"\d{4}-\d\d-\d\dT", l)]
inj = {"keyboard": 0, "tablet": 0}
tablet_times = []
for l in stamped:
    m = re.search(r'virtio-input event injected device="(\w+)"', l)
    if m:
        inj[m.group(1)] += 1
        if m.group(1) == "tablet":
            tablet_times.append(ts(l))
drops = [l for l in stamped if "virtio-input event dropped" in l]
stall_warns = [l for l in stamped if "has not presented its frame" in l or "session failed" in l]
presented = [(ts(l), l) for l in stamped if "frame presented sequence=" in l]
lat = [int(m.group(1)) for _, l in presented if (m := re.search(r"flush_to_present_us=(\d+)", l))]
print(f"desktop-stress: injected outside the burst keyboard={inj['keyboard']} tablet={inj['tablet']}; "
      f"dropped-event WARNs={len(drops)} (during the keygap-0 burst: {len(burst_drops)})", file=sys.stderr)
for l in burst_drops[:3]:
    print("  burst: " + l[l.index("virtio-input"):][:200], file=sys.stderr)
if not tablet_times or not presented:
    print("desktop-stress: no per-event or per-frame lines (STRESS_LOG); FPS and stall analysis skipped", file=sys.stderr)
    sys.exit(1 if drops or stall_warns else 0)
# The mouse phase: the longest run of tablet events with no gap above 0.3 s.
best, start = (0, 0), 0
for i in range(1, len(tablet_times) + 1):
    if i == len(tablet_times) or tablet_times[i] - tablet_times[i - 1] > 0.3:
        if i - start > best[1] - best[0]:
            best = (start, i)
        start = i
t0, t1 = tablet_times[best[0]], tablet_times[best[1] - 1]
window = [t for t, _ in presented if t0 <= t <= t1]
gaps = [b - a for a, b in zip(window, window[1:])]
fps = (len(window) - 1) / (window[-1] - window[0]) if len(window) > 1 else 0.0
gap_max = max(gaps) * 1000 if gaps else 0.0
# A stall: input kept arriving (event gaps under 50 ms) for over 200 ms with no
# frame presented. A frame gap while the pointer sits still is just idle.
phase = tablet_times[best[0]:best[1]]
over = 0
for a, b in zip(window, window[1:]):
    inside = [a] + [t for t in phase if a < t < b] + [b]
    run_start, longest = inside[0], 0.0
    for x, y in zip(inside, inside[1:]):
        if y - x >= 0.05:
            run_start = y
        longest = max(longest, y - run_start)
    over += longest > 0.2
print(f"desktop-stress: mouse phase {t1 - t0:.1f} s, {best[1] - best[0]} tablet events "
      f"({(best[1] - best[0]) / max(t1 - t0, 1e-6):.0f}/s): presented {len(window)} frames = {fps:.1f} fps "
      f"(cap {cap}); longest gap between presented frames {gap_max:.0f} ms; stalls under input over 200 ms: {over}", file=sys.stderr)
if lat:
    lat_sorted = sorted(lat)
    p50, p95 = lat_sorted[len(lat) // 2], lat_sorted[int(len(lat) * 0.95) - 1]
    print(f"desktop-stress: flush-to-present over the run: {len(lat)} samples, p50 {p50 / 1000:.1f} ms, "
          f"p95 {p95 / 1000:.1f} ms, max {max(lat) / 1000:.1f} ms", file=sys.stderr)
for l in [l for l in stamped if "display present window" in l or "display presenter stopped" in l]:
    print("  " + l[l.index("ternvale::gpu:"):][:240], file=sys.stderr)
for l in (drops + stall_warns)[:10]:
    print("  " + l[:240], file=sys.stderr)
bad = drops or stall_warns or over or fps < cap * 0.8
sys.exit(1 if bad else 0)
PY
log "passed; logs in $out"
