#!/bin/bash
# Ad-hoc codesign a binary with the Ternvale hypervisor entitlement, then exec it.
# Cargo passes the test or run binary as the first argument and the rest unchanged.
set -euo pipefail

log() {
    printf 'sign-and-run: %s\n' "$*" >&2
}

fail() {
    log "error: $*"
    exit 1
}

if [[ $# -lt 1 ]]; then
    fail "usage: sign-and-run.sh <binary> [args...]"
fi

binary=$1
shift

script_dir=$(cd "$(dirname "$0")" && pwd)
entitlements="${script_dir}/../entitlements/ternvale.entitlements"

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

log "codesign ad-hoc with com.apple.security.hypervisor"
if ! codesign --sign - --force --entitlements "$entitlements" "$binary"; then
    fail "codesign failed for ${binary}"
fi

log "exec ${binary}"
exec "$binary" "$@"
