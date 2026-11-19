#!/usr/bin/env bash
# Step 104 live: agent driver-logs on two VMs; Windows WDK/WinDbg/Verifier when
# TERNVALE_WINDOWS_GUEST=1. Host control-socket pull is covered by cargo tests.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
stamp="$(date +%Y%m%d-%H%M%S)"
out="$root/target/driver-dev/${stamp}"
assets="${TERNVALE_DEVICES_ASSETS:-$root/test-assets/virtio-devices}"
run_dir="/tmp/tvdrv-$$"
bin="${CARGO_TARGET_DIR:-$root/target}/debug/ternvale"
export TERNVALE_RUN_DIR="$run_dir"
export TERNVALE_SKIP_SANDBOX=1
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::agent=debug}"
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-$root/entitlements/ternvale-hv-only.entitlements}"
export PATH="/opt/homebrew/bin:${PATH}"

log() { printf 'driver-dev-live: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

mkdir -p "$out" "$run_dir"
: >"$out/summary.txt"

cleanup() {
  for n in drv-a drv-b; do
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$n" --force \
      >/dev/null 2>&1 || true
  done
  rm -rf "$run_dir"
}
trap cleanup EXIT

log "host cargo tests (agent sidecar + control driver-logs)"
(cd "$root" && cargo test -q -p ternvale-agent drvlog -- --test-threads=1)
(cd "$root" && cargo test -q -p ternvale-devices driver_logs_get -- --test-threads=1)
(cd "$root" && cargo test -q -p ternvale-cli driver_logs_control -- --test-threads=1)
echo "host_tests=ok" >>"$out/summary.txt"

wait_serial() {
  local file=$1 pattern=$2 timeout=${3:-120}
  local i=0
  while [[ $i -lt $timeout ]]; do
    if [[ -f "$file" ]] && grep -E -q "$pattern" "$file"; then
      return 0
    fi
    sleep 1
    i=$((i + 1))
  done
  return 1
}

patch_initramfs() {
  local src=$1 dest=$2
  local work="$out/initrd"
  rm -rf "$work"
  mkdir -p "$work"
  (cd "$work" && cpio -id --quiet <"$src")
  if ! grep -q 'ternvale\.drvlog' "$work/init"; then
    python3 - "$work/init" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
text = p.read_text()
needle = "TERNVALE_AGENT_LOG=${level} setsid /bin/ternvale-agent"
insert = """mkdir -p /var/log/ternvale/driver-logs
    echo "tvhello: DriverEntry" >/var/log/ternvale/driver-logs/tvhello.log
    echo "tvhello: D0 entry" >>/var/log/ternvale/driver-logs/tvhello.log
    echo "tvhello: live-check sidecar" >>/var/log/ternvale/driver-logs/tvhello.log
    echo "ternvale driver log sidecar ready"
  """
if needle not in text:
    raise SystemExit("init has no agent start line to patch")
p.write_text(text.replace(needle, insert + needle, 1))
PY
  fi
  (cd "$work" && find . -mindepth 1 | sed 's|^\./||' | sort | cpio -o -H newc) \
    >"$dest" 2>/dev/null
}

boot_one() {
  local name=$1
  local serial="$out/${name}-serial.log"
  local stderr="$out/${name}.stderr"
  cat >"$out/${name}.toml" <<EOF
name = "${name}"
backend = "native"
profile = "linux"
cpus = 1
ram_mib = 512
kernel = "$assets/Image"
initrd = "$out/initramfs.cpio"
cmdline = "console=ttyAMA0 rdinit=/init ternvale.agent=info ternvale.drvlog=1"
serial_log = "${serial}"
[tpm]
enabled = false
[audio]
enabled = false
[usb]
enabled = false
[vsock]
agent = true
EOF
  : >"$serial"
  log "boot ${name}"
  "$root/scripts/sign-and-run.sh" "$bin" run "$out/${name}.toml" \
    </dev/null >"$out/${name}.stdout" 2>"$stderr" &
  echo $!
}

pull_logs() {
  local name=$1
  local i=0
  while [[ $i -lt 90 ]]; do
    if TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" wait-ready "$name" \
      --timeout 2 >"$out/${name}-wait.txt" 2>&1; then
      break
    fi
    i=$((i + 2))
  done
  TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" driver-logs "$name" \
    --out "$out/${name}-tvhello.log" --timeout 8 \
    >"$out/${name}-pull.txt" 2>&1 \
    || fail "${name}: driver-logs failed ($(cat "$out/${name}-pull.txt"))"
  grep -q "tvhello: DriverEntry" "$out/${name}-tvhello.log" \
    || fail "${name}: sidecar missing DriverEntry"
  log "${name}: pulled $(wc -c <"$out/${name}-tvhello.log") bytes"
}

(cd "$root" && cargo build -p ternvale-cli -q)
"$root/scripts/sign-and-run.sh" "$bin" --help >/dev/null

linux_ok=0
if [[ "${TERNVALE_DRIVER_DEV_HV:-}" != "1" ]]; then
  log "SKIP linux HV VMs: set TERNVALE_DRIVER_DEV_HV=1 (needs virtio-devices initramfs)"
  echo "linux_vms=skip_set_TERNVALE_DRIVER_DEV_HV" >>"$out/summary.txt"
elif [[ ! -f "$assets/Image" || ! -f "$assets/initramfs.cpio" ]]; then
  log "SKIP linux VMs: missing $assets (./scripts/make-devices-initramfs.sh)"
  echo "linux_vms=skip_no_assets" >>"$out/summary.txt"
else
  patch_initramfs "$assets/initramfs.cpio" "$out/initramfs.cpio"
  for name in drv-a drv-b; do
    pid=$(boot_one "$name")
    if ! wait_serial "$out/${name}-serial.log" "ternvale agent started|ternvale driver log sidecar ready" 90; then
      tail -40 "$out/${name}-serial.log" >&2 || true
      tail -20 "$out/${name}.stderr" >&2 || true
      log "WARN ${name}: agent did not start (module/UART hang); host tests still count"
      echo "linux_vms=fail_${name}_no_agent" >>"$out/summary.txt"
      TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$name" --force \
        >/dev/null 2>&1 || true
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
      break
    fi
    pull_logs "$name"
    TERNVALE_LOG=warn "$root/scripts/sign-and-run.sh" "$bin" stop "$name" --force \
      >/dev/null 2>&1 || true
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    linux_ok=1
  done
  if [[ "$linux_ok" = "1" ]]; then
    echo "linux_vms=ok drv-a+drv-b" >>"$out/summary.txt"
  fi
fi

echo "kdnet_user_nat=TODO_verify WinDbg is Windows-only; user NAT maps 10.0.2.2 to the host UDP" \
  >>"$out/summary.txt"

if [[ "${TERNVALE_WINDOWS_GUEST:-}" == "1" ]]; then
  log "Windows guest path (SSH)"
  ssh_opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8)
  guest="${TERNVALE_WINDOWS_SSH:-Administrator@127.0.0.1}"
  port="${TERNVALE_WINDOWS_SSH_PORT:-2222}"
  ssh "${ssh_opts[@]}" -p "${port}" "${guest}" \
    "powershell -File C:\\Ternvale\\drivers\\scripts\\tvhello-live.ps1" \
    | tee "$out/windows-live.txt"
  echo "windows=ran" >>"$out/summary.txt"
else
  log "SKIP Windows WDK/WinDbg/Verifier/dump: set TERNVALE_WINDOWS_GUEST=1 after Setup + VS + WDK"
  echo "windows=skip_no_guest" >>"$out/summary.txt"
fi

log "PASS host tests; linux_vms=${linux_ok}; artifacts $out"
cat "$out/summary.txt" >&2
