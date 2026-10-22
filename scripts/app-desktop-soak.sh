#!/usr/bin/env bash
# Desktop guest (docs/DESKTOP.md) through the embedded display path (same TVDP
# socket + SET_SIZE as the macOS app). Validates render, input, dynamic resize
# without restarting the VM, then soaks with idle + periodic pointer motion while
# sampling helper RSS and checking presenter frame accounting in the host log.
#
# Usage: SOAK_SEC=600 FPS=30 ./scripts/app-desktop-soak.sh
#        SOAK_SEC=120  # shorter smoke
# Needs: test-assets/desktop, GUI session for the viewer window, hypervisor entitlement.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
assets="$root/test-assets/desktop"
target="${CARGO_TARGET_DIR:-$root/target}"
fps="${FPS:-30}"
soak_sec="${SOAK_SEC:-600}"
name="app-desktop-soak"
out="$root/target/app-desktop-soak/$(date +%Y%m%d-%H%M%S)-soak${soak_sec}"
run_dir="/tmp/tvapp-$$"
viewer="$target/viewer/ternvale-viewer"
socket="$run_dir/$name.display.sock"

log() { printf 'app-desktop-soak: %s\n' "$*" >&2; }

for f in Image initramfs.cpio rootfs.ext4; do
    [[ -f "$assets/$f" ]] || { log "missing $assets/$f; run scripts/make-desktop-rootfs.sh"; exit 1; }
done
"$root/scripts/ternvale-cli-build.sh" --quiet
"$root/scripts/build-viewer.sh"
mkdir -p "$out" "$run_dir"
cp -c "$assets/rootfs.ext4" "$out/rootfs.ext4" 2>/dev/null || cp "$assets/rootfs.ext4" "$out/rootfs.ext4"

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

{
    echo "wait-frame"
    echo "frame-info"
    echo "wait-file $out/resize-now"
    echo "set-size 1024 768"
    echo "wait-file $out/resize-check"
    echo "move 512 384"
    echo "sleep 500"
    echo "wait-file $out/soak-done"
} >"$out/selftest.txt"

fifo="$out/stdin"
mkfifo "$fifo"
export TERNVALE_RUN_DIR="$run_dir" TERNVALE_EMBEDDED_DISPLAY=1 \
    TERNVALE_VIEWER_SELFTEST="$out/selftest.txt" \
    TERNVALE_LOG="${SOAK_LOG:-info,ternvale::gpu=debug,ternvale::virtio::input=trace}"
"$root/scripts/sign-and-run.sh" "$target/debug/ternvale" run "$out/vm.toml" \
    --display window --fps "$fps" <"$fifo" >/dev/null 2>"$out/host.log" &
vmm=$!
exec 3>"$fifo"

viewer_pid=0

cleanup() {
    exec 3>&- || true
    if [[ "$viewer_pid" != 0 ]] && kill -0 "$viewer_pid" 2>/dev/null; then kill "$viewer_pid" 2>/dev/null || true; fi
    if kill -0 "$vmm" 2>/dev/null; then
        TERNVALE_LOG=warn "$target/debug/ternvale" stop "$name" >/dev/null 2>&1 || kill "$vmm" 2>/dev/null || true
        wait "$vmm" 2>/dev/null || true
    fi
    rm -rf "$run_dir"
}
trap cleanup EXIT

wait_for() {
    local i=0
    until [[ $(grep -acE -- "$2" "$1" 2>/dev/null) -ge ${4:-1} ]]; do
        kill -0 "$vmm" 2>/dev/null || { log "VMM exited; see $out/host.log"; exit 1; }
        ((i++ >= $3 * 10)) && { log "timed out waiting for '$2' in $1"; exit 1; }
        sleep 0.1
    done
}
serial_send() {
    local cmd="$1" k
    for ((k = 0; k < ${#cmd}; k++)); do
        printf '%s' "${cmd:k:1}" >&3
        sleep 0.005
    done
    printf '\n' >&3
}
serial_line() { grep -a "$1" "$out/serial.log" | tail -1 | tr -d '\r'; }

wait_for "$out/host.log" "display presenter ready for embedded" 30
[[ -S "$socket" ]] || { log "missing display socket $socket"; exit 1; }
"$viewer" --socket "$socket" --title "Ternvale app soak" >/dev/null 2>"$out/viewer.log" &
viewer_pid=$!
wait_for "$out/host.log" "display viewer hello accepted" 30
wait_for "$out/serial.log" "ternvale desktop ready" 120
grep -aq "ternvale desktop failed" "$out/serial.log" && { log "desktop failed; see serial.log"; exit 1; }
log "guest: $(serial_line 'ternvale desktop ready' | sed 's/.*ternvale //')"

# Initial screen (1280x800)
serial_send "export DISPLAY=:0"
serial_send "xdpyinfo | sed -n '1,12p' | tr '\\n' ' ' | sed 's/^/xdpy: /'; echo"
wait_for "$out/serial.log" "^xdpy: " 15
log "before resize: $(serial_line '^xdpy: ')"

touch "$out/resize-now"
wait_for "$out/host.log" "selftest set-size width=1024 height=768" 30
sleep 2
serial_send "xdpyinfo | grep dimensions | sed 's/^/xdpy: /'; echo"
wait_for "$out/serial.log" "dimensions:.*1024x768" 30
log "after resize: $(serial_line 'dimensions:')"
touch "$out/resize-check"
grep -aq "1024x768" "$out/serial.log" || { log "guest did not report 1024x768 after SET_SIZE"; exit 1; }
log "resize: guest display changed without VM restart"

printf 'elapsed_sec\trss_kb\n' >"$out/rss.tsv"
soak_start=$SECONDS
end=$((soak_start + soak_sec))
cycle=0
while (( SECONDS < end )); do
    cycle=$((cycle + 1))
    if rss=$(ps -o rss= -p "$vmm" 2>/dev/null | tr -d ' '); then
        printf '%d\t%s\n' "$((SECONDS - soak_start))" "$rss" >>"$out/rss.tsv"
    fi
    serial_send "xwhere"
    sleep 15
    serial_send "export DISPLAY=:0; xdotool mousemove $((200 + cycle % 400)) $((200 + cycle % 300)) 2>/dev/null || true"
    sleep 15
done
touch "$out/soak-done"
wait_for "$out/host.log" "selftest script done" 120
log "soak: ${soak_sec}s elapsed ($cycle idle/move cycles)"

TERNVALE_LOG=warn "$target/debug/ternvale" stop "$name" >/dev/null
wait "$vmm" || true
viewer_pid=0

plain="$out/host.plain.log"
sed $'s/\x1b\\[[0-9;]*m//g' "$out/host.log" >"$plain"
python3 - "$plain" "$out/rss.tsv" "$soak_sec" <<'PY'
import re, sys
host, rss_path, soak = sys.argv[1], sys.argv[2], int(sys.argv[3])
lines = open(host, errors="replace").read().splitlines()
imbalance = [l for l in lines if "display IPC frame imbalance" in l]
orphan = [l for l in lines if "still awaiting a presented ack" in l]
stall = [l for l in lines if "has not presented its frame" in l]
stopped = [l for l in lines if "display presenter stopped" in l]
print(f"app-desktop-soak: IPC imbalance WARNs={len(imbalance)} orphan-in-flight WARNs={len(orphan)} stall WARNs={len(stall)}")
for l in imbalance + orphan[:3]:
    print("  " + l[:240])
if stopped:
    print("  " + stopped[-1][stopped[-1].find("ternvale::gpu:"):][:240])
rss = []
try:
    rows = open(rss_path).read().splitlines()[1:]
    for row in rows:
        parts = row.split()
        if len(parts) >= 2:
            rss.append(int(parts[1]))
except OSError:
    pass
if len(rss) >= 2:
    growth = rss[-1] - rss[0]
    peak = max(rss)
    print(f"app-desktop-soak: helper RSS start={rss[0]} KiB end={rss[-1]} KiB peak={peak} KiB growth={growth} KiB over {soak}s")
    if growth > 50_000:
        print("app-desktop-soak: WARN helper RSS grew more than 50 MiB during soak", file=sys.stderr)
        sys.exit(1)
bad = bool(imbalance or orphan)
sys.exit(1 if bad else 0)
PY
log "passed; artifacts in $out"
