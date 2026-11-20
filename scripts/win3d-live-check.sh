#!/usr/bin/env bash
# Step 108 test: 2D fallback, software-vs-Metal logs, start/stop leak analog,
# optional 30m soak, optional Windows guest vkcube/DXVK (TERNVALE_WINDOWS_GUEST=1).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/target/win3d-live/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out"
cd "$root"
summary="$out/summary.txt"
: >"$summary"
note() { printf '%s\n' "$*" | tee -a "$summary" >&2; }

note "fallback_2d: gpu3d=false rejects CTX_CREATE, GET_DISPLAY_INFO still OK"
cargo test -p ternvale-gpu gpu3d_runtime_off -- --test-threads=1
note "fallback_2d=ok"

note "host_path: Venus without virgl stays software_stub; tvvk DRAW_CUBE uses MoltenVK"
cargo test -p ternvale-gpu3d-host host_gpu_without_virgl_logs_software_not_metal \
  -- --test-threads=1 --nocapture | tee "$out/host-path.log"
TERNVALE_ENTITLEMENTS="$root/entitlements/ternvale-gpu3d.entitlements" \
  cargo test -p ternvale-gpu3d-host host_gpu_draw_cube_uses_moltenvk_when_present \
  -- --test-threads=1 --nocapture | tee "$out/moltenvk-path.log"
if grep -F 'host_gpu_path=moltenvk' "$out/moltenvk-path.log" >/dev/null 2>&1 \
  && grep -F 'metal=true' "$out/moltenvk-path.log" >/dev/null 2>&1; then
  note "host_metal=yes (MoltenVK instance for tvvk DRAW_CUBE)"
else
  note "host_metal=no (libMoltenVK.dylib missing or vkCreateInstance failed)"
fi
note "host_path=ok"

note "leak analog: 256 start/stop cycles"
cargo test -p ternvale-gpu3d-host win3d_app_start_stop_does_not_leak_host_objects \
  -- --test-threads=1
note "leak_analog=ok"

note "icd proto"
./scripts/win3d-proto-check.sh | tee "$out/proto.log"
note "icd_proto=ok"

soak_secs="${TERNVALE_WIN3D_SOAK_SECS:-1800}"
if [[ "$soak_secs" == "0" ]]; then
  note "soak_30m=skip (TERNVALE_WIN3D_SOAK_SECS=0)"
else
  note "soak_30m: start/stop ${soak_secs}s"
  TERNVALE_WIN3D_SOAK_SECS="$soak_secs" cargo test -p ternvale-gpu3d-host \
    win3d_start_stop_thirty_minutes -- --ignored --test-threads=1 --nocapture \
    | tee "$out/soak.log"
  note "soak_30m=ok secs=$soak_secs (host analog; no Windows VM)"
fi

if [[ "${TERNVALE_WINDOWS_GUEST:-}" == "1" ]]; then
  note "windows_guest: vkcube.exe / DXVK triangle via SSH"
  ssh_opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8)
  guest="${TERNVALE_WINDOWS_SSH:-Administrator@127.0.0.1}"
  port="${TERNVALE_WINDOWS_SSH_PORT:-2222}"
  if ssh "${ssh_opts[@]}" -p "${port}" "${guest}" \
      "powershell -File C:\\Ternvale\\guest-tools\\win3d\\tvvk-live.ps1" \
      | tee "$out/windows-guest.log"; then
    note "windows_guest=ok (inspect vkcube=/d3d11=/dxvk= lines)"
    grep -E 'vkcube=|d3d11=|dxvk=|metal=|fallback=' "$out/windows-guest.log" \
      | tee -a "$summary" || true
  else
    note "windows_guest=fail (see $out/windows-guest.log)"
    note "vkcube=fail"
    note "d3d11_dxvk=fail"
  fi
else
  note "windows_guest=skip (set TERNVALE_WINDOWS_GUEST=1 after Win11 ARM64 + tvvk)"
  note "vkcube=not_run"
  note "d3d11_dxvk=not_run"
fi

note "gpu_acceleration_doc=docs/GPU_ACCELERATION.md Step 108 (ICD, licenses, works/doesn't, remaining work)"
note "artifacts $out"
cat "$summary"
