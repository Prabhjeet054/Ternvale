#!/usr/bin/env bash
# Step 105 live: host resize-storm / two-scanout analog always; Windows KMDOD
# install + Device Manager + SDV + 1h Verifier when TERNVALE_WINDOWS_GUEST=1.
# Optional TVDP SET_SIZE storm if a presenter socket exists.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
stamp="$(date +%Y%m%d-%H%M%S)"
out="$root/target/tvgpu-dod/${stamp}"
export PATH="/opt/homebrew/bin:${PATH}"
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::gpu=debug}"

log() { printf 'tvgpu-dod-live: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

mkdir -p "$out"
: >"$out/summary.txt"

log "host analog: 400 resizes, two scanouts, D3/D0 present"
(cd "$root" && cargo test -q -p ternvale-gpu tvgpu_dod_live -- --test-threads=1) \
  || fail "ternvale-gpu tvgpu_dod_live tests"
echo "host_analog=ok" >>"$out/summary.txt"

# TVDP SET_SIZE: HELLO then hundreds of resizes (scanout 0 only; app is one display).
resize_storm() {
  local sock=$1
  python3 - "$sock" "$out/set-size-storm.txt" <<'PY'
import os, socket, struct, sys, time
path, log_path = sys.argv[1], sys.argv[2]
MAGIC = int.from_bytes(b"TVDP", "little")
VERSION = 2
HELLO, SET_SIZE = 0x0101, 0x0205

def msg(kind, value):
    return struct.pack("<IHHQ", MAGIC, VERSION, kind, value)

sizes = [(800, 600), (1024, 768), (1280, 800), (1920, 1080)]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(5)
s.connect(path)
s.sendall(msg(HELLO, VERSION))
n = 0
for i in range(400):
    w, h = sizes[i % len(sizes)]
    s.sendall(msg(SET_SIZE, w | (h << 32)))
    n += 1
    time.sleep(0.01)
s.close()
open(log_path, "w").write(f"set_size_count={n}\n")
print(f"set_size_count={n}")
PY
}

sock="${TERNVALE_TVGPU_DISPLAY_SOCK:-}"
if [[ -z "$sock" ]]; then
  run="${TERNVALE_RUN_DIR:-$HOME/Library/Application Support/Ternvale/run}"
  name="${TERNVALE_TVGPU_VM:-}"
  if [[ -n "$name" && -S "$run/${name}.display.sock" ]]; then
    sock="$run/${name}.display.sock"
  fi
fi
if [[ -n "${sock}" && -S "$sock" ]]; then
  log "TVDP SET_SIZE storm on $sock"
  resize_storm "$sock"
  echo "set_size_storm=ok sock=$sock" >>"$out/summary.txt"
else
  log "SKIP SET_SIZE storm: no presenter socket (set TERNVALE_TVGPU_DISPLAY_SOCK or TERNVALE_TVGPU_VM)"
  echo "set_size_storm=skip_no_socket" >>"$out/summary.txt"
fi

echo "app_two_displays=unsupported_native_gpu_has_N_scanouts_app_presenter_is_scanout0" \
  >>"$out/summary.txt"

if [[ "${TERNVALE_WINDOWS_GUEST:-}" == "1" ]]; then
  log "Windows guest SSH: install + Device Manager + SDV + Verifier stress"
  ssh_opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8)
  guest="${TERNVALE_WINDOWS_SSH:-Administrator@127.0.0.1}"
  port="${TERNVALE_WINDOWS_SSH_PORT:-2222}"
  ssh "${ssh_opts[@]}" -p "${port}" "${guest}" \
    "powershell -File C:\\Ternvale\\drivers\\scripts\\tvgpu-dod-live.ps1" \
    | tee "$out/windows-live.txt"
  echo "windows=ran" >>"$out/summary.txt"
else
  log "SKIP Windows KMDOD install/SDV/Verifier/1h: set TERNVALE_WINDOWS_GUEST=1"
  echo "windows=skip_no_guest" >>"$out/summary.txt"
fi

log "PASS artifacts $out"
cat "$out/summary.txt" >&2
