# Shared codesign identity picker. Source from other scripts (not exec).
# Order: env → Developer ID Application → TERNVALE_TEAM_ID → preferred
# Apple Development email → first Apple Development → ad-hoc "-".
# A foreign Apple Development cert in Keychain-first order AMFI-SIGKILLs HV.

ternvale_first_identity_matching() {
    local needle=$1
    security find-identity -v -p codesigning 2>/dev/null \
        | sed -n 's/.*"\(.*\)"/\1/p' \
        | grep -F -- "$needle" \
        | head -n 1 || true
}

ternvale_pick_codesign_identity() {
    if [[ -n "${TERNVALE_CODESIGN_IDENTITY:-}" ]]; then
        printf '%s\n' "$TERNVALE_CODESIGN_IDENTITY"
        return
    fi
    local id
    id=$(ternvale_first_identity_matching "Developer ID Application:")
    if [[ -n "$id" ]]; then
        printf '%s\n' "$id"
        return
    fi
    if [[ -n "${TERNVALE_TEAM_ID:-}" ]]; then
        id=$(ternvale_first_identity_matching "(${TERNVALE_TEAM_ID})")
        if [[ -n "$id" ]]; then
            printf '%s\n' "$id"
            return
        fi
    fi
    # Default to this workspace's Apple Development identity. Override with
    # TERNVALE_CODESIGN_EMAIL. Keychain listing order is not stable.
    local email="${TERNVALE_CODESIGN_EMAIL:-prabhjeetsingh054@gmail.com}"
    id=$(ternvale_first_identity_matching "$email")
    if [[ -n "$id" ]]; then
        printf '%s\n' "$id"
        return
    fi
    id=$(ternvale_first_identity_matching "Apple Development:")
    if [[ -n "$id" ]]; then
        printf '%s\n' "$id"
        return
    fi
    printf '%s\n' "-"
}
