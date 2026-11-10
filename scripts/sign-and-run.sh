#!/bin/bash
# Codesign a binary with the hypervisor entitlement, then exec it.
# Cargo passes the test or run binary as the first argument and the rest unchanged.
#
# Default entitlements are hypervisor-only. `com.apple.vm.device-access` in
# entitlements/ternvale.entitlements SIGKILLs Apple Development CLI tools
# (no provisioning profile). USB live checks must set TERNVALE_ENTITLEMENTS.
set -euo pipefail

log() {
    printf 'sign-and-run: %s\n' "$*" >&2
}

fail() {
    log "error: $*"
    exit 1
}

script_dir=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib-codesign.sh
source "${script_dir}/lib-codesign.sh"

if [[ $# -lt 1 ]]; then
    fail "usage: sign-and-run.sh <binary> [args...]"
fi

binary=$1
shift

entitlements="${TERNVALE_ENTITLEMENTS:-${script_dir}/../entitlements/ternvale-hv-only.entitlements}"

log "binary=${binary}"
if [[ ! -f "$binary" ]]; then
    fail "binary does not exist: ${binary}"
fi
if [[ ! -x "$binary" ]]; then
    fail "binary is not executable: ${binary}"
fi

log "entitlements=${entitlements}"
if [[ ! -f "$entitlements" ]]; then
    fail "entitlements file does not exist: ${entitlements}"
fi

identity=$(ternvale_pick_codesign_identity)
log "codesign identity=${identity}"
sign=(--sign "$identity" --force --entitlements "$entitlements")
if [[ "$identity" != "-" ]]; then
    sign+=(--timestamp --options runtime)
fi
if ! codesign "${sign[@]}" "$binary"; then
    fail "codesign failed for ${binary}"
fi

if [[ -n "${TERNVALE_SEATBELT_PROFILE:-}" ]]; then
    if [[ ! -f "${TERNVALE_SEATBELT_PROFILE}" ]]; then
        fail "Seatbelt profile does not exist: ${TERNVALE_SEATBELT_PROFILE}"
    fi
    log "sandbox-exec -f ${TERNVALE_SEATBELT_PROFILE}"
    if [[ "${TERNVALE_SANDBOX_PROBE:-}" == "1" ]]; then
        export TERNVALE_SANDBOXED=1
    fi
    exec /usr/bin/sandbox-exec -f "${TERNVALE_SEATBELT_PROFILE}" "$binary" "$@"
fi

log "exec ${binary}"
set +e
"$binary" "$@"
status=$?
set -e
if [[ "$status" -eq 137 || "$status" -eq 9 ]]; then
    log "AMFI SIGKILL (exit ${status}). Usual causes:"
    log "  1. entitlements include com.apple.vm.device-access without a profile"
    log "     (default is entitlements/ternvale-hv-only.entitlements)"
    log "  2. codesign identity is not this Mac's Apple Development / Developer ID"
    log "  3. ad-hoc sign (-) on recent macOS"
fi
exit "$status"
