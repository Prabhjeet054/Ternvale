#!/bin/bash
# Codesign a binary with the Ternvale hypervisor entitlement, then exec it.
# Cargo passes the test or run binary as the first argument and the rest unchanged.
# Ad-hoc (`-`) is SIGKILL'd by AMFI on this OS; prefer a Keychain identity.
set -euo pipefail

log() {
    printf 'sign-and-run: %s\n' "$*" >&2
}

fail() {
    log "error: $*"
    exit 1
}

first_identity_matching() {
    local needle=$1
    security find-identity -v -p codesigning 2>/dev/null \
        | sed -n 's/.*"\(.*\)"/\1/p' \
        | grep -F -- "$needle" \
        | head -n 1 || true
}

pick_identity() {
    if [[ -n "${TERNVALE_CODESIGN_IDENTITY:-}" ]]; then
        printf '%s\n' "$TERNVALE_CODESIGN_IDENTITY"
        return
    fi
    local id
    id=$(first_identity_matching "Apple Development:")
    if [[ -n "$id" ]]; then
        printf '%s\n' "$id"
        return
    fi
    printf '%s\n' "-"
}

if [[ $# -lt 1 ]]; then
    fail "usage: sign-and-run.sh <binary> [args...]"
fi

binary=$1
shift

script_dir=$(cd "$(dirname "$0")" && pwd)
entitlements="${TERNVALE_ENTITLEMENTS:-${script_dir}/../entitlements/ternvale.entitlements}"

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

identity=$(pick_identity)
log "codesign identity=${identity} (com.apple.security.hypervisor)"
if ! codesign --sign "$identity" --force --entitlements "$entitlements" "$binary"; then
    fail "codesign failed for ${binary}"
fi

log "exec ${binary}"
exec "$binary" "$@"
