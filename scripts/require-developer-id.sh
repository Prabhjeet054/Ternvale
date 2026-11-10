#!/bin/bash
# Exit 0 if a Developer ID Application identity is in the Keychain.
# Gatekeeper (`spctl`) and notarytool cannot succeed without it.
set -euo pipefail

log() { printf 'require-developer-id: %s\n' "$*" >&2; }

id=$(security find-identity -v -p codesigning 2>/dev/null \
    | sed -n 's/.*"\(.*\)"/\1/p' \
    | grep -F "Developer ID Application:" \
    | head -n 1 || true)

if [[ -n "${TERNVALE_CODESIGN_IDENTITY:-}" ]]; then
    case "${TERNVALE_CODESIGN_IDENTITY}" in
        Developer\ ID\ Application:*)
            log "TERNVALE_CODESIGN_IDENTITY is Developer ID"
            printf '%s\n' "${TERNVALE_CODESIGN_IDENTITY}"
            exit 0
            ;;
    esac
fi

if [[ -n "$id" ]]; then
    log "found ${id}"
    printf '%s\n' "$id"
    exit 0
fi

log "no Developer ID Application certificate in this Keychain."
log "Apple Development can run HV tests (hv-only entitlements) but:"
log "  - spctl -a -vvv is always rejected"
log "  - notarytool will not accept the zip"
log "Create one at https://developer.apple.com/account/resources/certificates/list"
log "  Certificate type: Developer ID Application  (Apple Developer Program)"
log "Then: security find-identity -v -p codesigning"
log "Export TERNVALE_CODESIGN_IDENTITY and TERNVALE_NOTARY_* as in docs/RELEASE.md"
exit 1
