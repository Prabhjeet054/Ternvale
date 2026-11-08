#!/bin/bash
# Step 77 live: Linux sleep/wake date skew (SIGUSR1/2, 120s default),
# graceful stop with the guest agent, and force-stop when KEY_POWER is ignored.
#
# Does NOT put the Mac to sleep (`pmset sleepnow` would kill this session).
# Darwin SIGUSR1 = WillSleep, SIGUSR2 = DidWake on the `ternvale run` PID.
#
# Windows: skipped unless TERNVALE_WINDOWS_DISK points at an installed image
# (the HV test still records the skip; a full Windows boot is not automated).
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
export PATH="/opt/homebrew/bin:${HOME}/.cargo/bin:${PATH}"
export TERNVALE_POWER_SLEEP_SECS="${TERNVALE_POWER_SLEEP_SECS:-120}"
# hv-only: `vm.device-access` on Apple Development without a profile is SIGKILL.
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-${root}/entitlements/ternvale-hv-only.entitlements}"

log() { printf 'power-live: %s\n' "$*" >&2; }

log "unit: graceful_stop force-stop on a running (no-button) VM"
cargo test -p ternvale-cli --manifest-path "${root}/Cargo.toml" \
  --config 'target.aarch64-apple-darwin.runner="/usr/bin/env"' -- \
  --test-threads=1 graceful_stop_force_stops_when_a_running_guest_ignores_the_button

log "HV: ${TERNVALE_POWER_SLEEP_SECS}s simulated sleep + Linux graceful + force-stop fallback"
cargo test -p ternvale-cli --manifest-path "${root}/Cargo.toml" --test power -- \
  --ignored --nocapture --test-threads=1

log "PASS Step 77 live (Mac not slept; see target/control-logs/*/date-delta.txt)"
