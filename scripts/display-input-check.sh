#!/usr/bin/env bash
# Step 45 end-to-end input check through `ternvale run --display window`.
#
# A stand-in viewer (Python, via TERNVALE_VIEWER) speaks wire version 2: it
# acks frames like the real viewer, then types `echo typed-$((6*7)) >/dev/ttyAMA0`
# as macOS key codes. The VMM maps them to Linux keys and feeds its virtio-input
# keyboard; a shell the harness started on tty1 (the framebuffer console)
# runs the command, so "typed-42" on the serial log proves the whole path
# viewer -> presenter -> keymap -> virtio-input -> Linux keyboard -> VT.
# It then moves the pointer, clicks and scrolls; the host log must show the
# tablet's events injected at TRACE, and no drops.
#
# Needs the assets from scripts/make-gpu-initramfs.sh. No GUI session needed.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
assets="$root/test-assets/virtio-gpu"
name="display-input"
out="$root/target/display-input/$(date +%Y%m%d-%H%M%S)"
run_dir="/tmp/tvin-$$"

log() { printf 'display-input-check: %s\n' "$*" >&2; }

for f in Image initramfs.cpio; do
    [[ -f "$assets/$f" ]] || { log "missing $assets/$f; run scripts/make-gpu-initramfs.sh"; exit 1; }
done
(cd "$root" && cargo build -p ternvale-cli --quiet)
mkdir -p "$out" "$run_dir"

cat >"$out/fake-viewer.py" <<'PY'
import os, socket, struct, sys, threading, time
MAGIC, VERSION = 0x50445654, 2
sock = socket.socket(socket.AF_UNIX)
sock.connect(sys.argv[sys.argv.index("--socket") + 1])
lock = threading.Lock()
def send(kind, value):
    with lock:
        sock.sendall(struct.pack("<IHHQ", MAGIC, VERSION, kind, value))
def recv(n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            os._exit(0)
        buf += chunk
    return buf
def frames():
    while True:
        h = recv(40)
        seq, plen = struct.unpack_from("<Q", h, 24)[0], struct.unpack_from("<I", h, 32)[0]
        recv(plen)
        send(0x0102, seq)
send(0x0101, VERSION)
threading.Thread(target=frames, daemon=True).start()
trigger = os.environ["FAKE_VIEWER_TRIGGER"]
while not os.path.exists(trigger):
    time.sleep(0.1)
# kVK_* codes (HIToolbox/Events.h); (code, shift).
keys = {c: k for c, k in zip("asdfhgzxcv", [0, 1, 2, 3, 4, 5, 6, 7, 8, 9])}
keys.update({"b": 0x0B, "q": 0x0C, "w": 0x0D, "e": 0x0E, "r": 0x0F, "y": 0x10, "t": 0x11,
             "o": 0x1F, "u": 0x20, "i": 0x22, "p": 0x23, "l": 0x25, "j": 0x26, "k": 0x28,
             "n": 0x2D, "m": 0x2E, " ": 0x31, "-": 0x1B, "/": 0x2C, ".": 0x2F, "\n": 0x24,
             "0": 0x1D, "1": 0x12, "2": 0x13, "3": 0x14, "4": 0x15, "5": 0x17, "6": 0x16,
             "7": 0x1A, "8": 0x1C, "9": 0x19})
shifted = {"$": "4", "(": "9", ")": "0", "*": "8", ">": "."}
def key(code, pressed):
    send(0x0201, code | (int(pressed) << 16))
    time.sleep(0.01)
for ch in "echo typed-$((6*7)) >/dev/ttyAMA0\n":
    shift = ch in shifted or ch.isupper()
    code = keys[shifted.get(ch, ch.lower())]
    if shift:
        key(0x38, True)
    key(code, True)
    key(code, False)
    if shift:
        key(0x38, False)
for x, y in [(0, 0), (16384, 8192), (32767, 32767)]:
    send(0x0202, x | (y << 32))
send(0x0203, 0 | (1 << 8))
send(0x0203, 0)
send(0x0204, (1 << 32))
time.sleep(1)
PY
cat >"$out/viewer.sh" <<EOF
#!/bin/sh
exec python3 "$out/fake-viewer.py" "\$@"
EOF
chmod 755 "$out/viewer.sh"

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
export TERNVALE_RUN_DIR="$run_dir" TERNVALE_VIEWER="$out/viewer.sh" \
    FAKE_VIEWER_TRIGGER="$out/type-now" TERNVALE_LOG="info,ternvale::gpu=debug,ternvale::virtio::input=trace"
"$root/scripts/sign-and-run.sh" "$root/target/debug/ternvale" run "$out/vm.toml" \
    --display window <"$fifo" >/dev/null 2>"$out/host.log" &
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
    until grep -q -- "$2" "$1" 2>/dev/null; do
        kill -0 "$vmm" 2>/dev/null || { log "VMM exited early; see $out/host.log"; exit 1; }
        ((i++ >= $3 * 10)) && { log "timed out waiting for '$2' in $1"; exit 1; }
        sleep 0.1
    done
}
wait_for "$out/host.log" "display viewer hello accepted" 20
wait_for "$out/serial.log" "ternvale gpu ready fb0=virtio_gpudrmfb" 30
sleep 1
# One byte at a time: the PL011 rx FIFO drops bytes that arrive faster than
# the guest drains it.
cmd="setsid sh -c 'exec sh -i </dev/tty1 >/dev/tty1 2>&1' &"
for ((k = 0; k < ${#cmd}; k++)); do
    printf '%s' "${cmd:k:1}" >&3
    sleep 0.005
done
printf '\n' >&3
sleep 2
touch "$out/type-now"
wait_for "$out/serial.log" "typed-42" 20
log "guest shell on tty1 ran the typed command: $(grep -m1 -a 'typed-42' "$out/serial.log" | tr -d '\r')"
sleep 2
TERNVALE_LOG=warn "$root/target/debug/ternvale" stop "$name" >/dev/null
wait "$vmm" || true

plain="$out/host.plain.log"
sed $'s/\x1b\\[[0-9;]*m//g' "$out/host.log" >"$plain"
kbd=$(grep -c 'virtio-input event injected.*device="keyboard"' "$plain" || true)
tab=$(grep -c 'virtio-input event injected.*device="tablet"' "$plain" || true)
drops=$(grep -c 'virtio-input event dropped' "$plain" || true)
log "TRACE injected: keyboard=$kbd tablet=$tab; WARN drops=$drops"
grep -E 'attached virtio-input|display presenter stopped' "$plain" | sed 's/^/  /' >&2
# 34 typed characters, 10 with shift: (34 + 10) * 2 key events, each + SYN.
# Tablet: 3 moves (3 events each), 2 button reports and one wheel report (2 each).
[[ "$kbd" -eq 176 && "$tab" -eq 15 && "$drops" -eq 0 ]] || { log "unexpected counts; see $plain"; exit 1; }
log "passed; logs in $out"
