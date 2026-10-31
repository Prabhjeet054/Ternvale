#!/usr/bin/env bash
# Release pipeline: release-mode aarch64 helper, embed in Ternvale.app, sign with
# the hypervisor entitlement, optional notarytool submit.
#
# Dry-run: if TERNVALE_CODESIGN_IDENTITY is unset, pick a Keychain identity
# (Developer ID preferred, else Apple Development). If App Store Connect API
# credentials are missing, notarization is skipped and logged — it does not fail.
# See docs/RELEASE.md. Never pass secrets as argv.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
entitlements="${root}/entitlements/ternvale.entitlements"
proj="${root}/app/Ternvale/Ternvale.xcodeproj"
target_triple="aarch64-apple-darwin"
skip_notarize=0
skip_flag=0
out="${root}/dist"

usage() {
    cat <<'EOF'
usage: scripts/release.sh [--skip-notarize] [--out DIR]

Build a signed Ternvale.app for aarch64-apple-darwin.

Environment (docs/RELEASE.md; never commit values):
  TERNVALE_CODESIGN_IDENTITY   Developer ID Application identity (optional; auto-picked)
  TERNVALE_TEAM_ID             Apple Developer Team ID (logged only)
  TERNVALE_NOTARY_KEY_ID       App Store Connect API key id
  TERNVALE_NOTARY_ISSUER       App Store Connect Issuer ID (UUID)
  TERNVALE_NOTARY_KEY_PATH     Path to AuthKey_<id>.p8
EOF
}

log() {
    printf 'release: %s\n' "$*" >&2
}

fail() {
    log "error: $*"
    exit 1
}

# First codesigning identity whose name contains $1 (substring of the quoted name).
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
    id=$(first_identity_matching "Developer ID Application:")
    if [[ -n "$id" ]]; then
        printf '%s\n' "$id"
        return
    fi
    id=$(first_identity_matching "Apple Development:")
    if [[ -n "$id" ]]; then
        printf '%s\n' "$id"
        return
    fi
    printf '%s\n' "-"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-notarize|--dry-run) skip_flag=1; skip_notarize=1; shift ;;
        --out) out="${2:?}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown argument: $1" ;;
    esac
done

[[ -f "$entitlements" ]] || fail "missing entitlements at $entitlements"

identity=$(pick_identity)
team="${TERNVALE_TEAM_ID:-}"
sign_args=(--force --options runtime --entitlements "$entitlements" --sign "$identity")
if [[ "$identity" == "-" ]]; then
    log "TERNVALE_CODESIGN_IDENTITY unset and no Keychain identity; ad-hoc dry-run (-)"
else
    log "codesign identity: ${identity}"
    if [[ "$identity" != Developer\ ID\ Application:* ]]; then
        log "identity is not Developer ID Application; Gatekeeper/spctl will fail until a Developer ID + notarize + staple"
        skip_notarize=1
        log "skipping notarization (Developer ID required; this identity cannot be notarized)"
    fi
    sign_args+=(--timestamp)
fi
if [[ -z "$team" ]]; then
    log "TERNVALE_TEAM_ID unset; not written into the binary"
fi

key_id="${TERNVALE_NOTARY_KEY_ID:-}"
issuer="${TERNVALE_NOTARY_ISSUER:-}"
key_path="${TERNVALE_NOTARY_KEY_PATH:-}"
if [[ "$skip_flag" -eq 1 ]]; then
    log "skipping notarization (--skip-notarize/--dry-run)"
elif [[ "$skip_notarize" -eq 0 ]]; then
    if [[ -z "$key_id" || -z "$issuer" || -z "$key_path" ]]; then
        log "skipping notarization (no App Store Connect API credentials: set TERNVALE_NOTARY_KEY_ID, TERNVALE_NOTARY_ISSUER, TERNVALE_NOTARY_KEY_PATH)"
        skip_notarize=1
    elif [[ ! -f "$key_path" ]]; then
        log "skipping notarization (TERNVALE_NOTARY_KEY_PATH is not a file: ${key_path})"
        skip_notarize=1
    fi
fi

mkdir -p "$out"
# Pin the target dir so a parent CARGO_TARGET_DIR (CI/sandbox) cannot hide the helper.
export CARGO_TARGET_DIR="${TERNVALE_CARGO_TARGET_DIR:-${root}/target}"
log "cargo build --release --target ${target_triple} -p ternvale-cli (CARGO_TARGET_DIR=${CARGO_TARGET_DIR})"
(
    cd "$root"
    cargo build --release --target "$target_triple" -p ternvale-cli
)
helper="${CARGO_TARGET_DIR}/${target_triple}/release/ternvale"
if [[ ! -x "$helper" ]]; then
    helper="${CARGO_TARGET_DIR}/release/ternvale"
fi
[[ -x "$helper" ]] || fail "release helper missing under ${CARGO_TARGET_DIR}"

derived="${out}/DerivedData"
app_dir="${out}/app"
rm -rf "$derived" "$app_dir"
mkdir -p "$app_dir"
log "xcodebuild Release (unsigned; we sign after embedding the helper)"
xcodebuild \
    -project "$proj" \
    -scheme Ternvale \
    -configuration Release \
    -destination 'generic/platform=macOS' \
    -derivedDataPath "$derived" \
    ARCHS=arm64 \
    ONLY_ACTIVE_ARCH=NO \
    CONFIGURATION_BUILD_DIR="$app_dir" \
    CODE_SIGNING_ALLOWED=NO \
    CODE_SIGNING_REQUIRED=NO \
    CODE_SIGN_IDENTITY="" \
    DEVELOPMENT_TEAM="" \
    TERNVALE_HELPER_SRC="$helper" \
    build

app="${app_dir}/Ternvale.app"
[[ -d "$app" ]] || fail "xcodebuild did not produce $app"

export TERNVALE_HELPER_SRC="$helper"
"${root}/app/scripts/copy-ternvale-helper.sh" "$app"
embedded="${app}/Contents/Helpers/ternvale"
[[ -x "$embedded" ]] || fail "helper not embedded at $embedded"

log "codesign helper (hardened runtime, hypervisor entitlement)"
codesign "${sign_args[@]}" "$embedded"
log "codesign app bundle (hardened runtime, hypervisor entitlement)"
codesign "${sign_args[@]}" "$app"

log "codesign --verify --deep --strict"
codesign --verify --deep --strict --verbose=2 "$app"
codesign --verify --deep --strict --verbose=2 "$embedded"

log "entitlements (app executable)"
codesign --display --entitlements :- "$app/Contents/MacOS/Ternvale" 2>/dev/null \
    | tee "${out}/app-entitlements.xml" >&2
log "entitlements (embedded helper)"
codesign --display --entitlements :- "$embedded" 2>/dev/null \
    | tee "${out}/helper-entitlements.xml" >&2
grep -q "com.apple.security.hypervisor" "${out}/app-entitlements.xml" \
    || fail "app lost com.apple.security.hypervisor"
grep -q "com.apple.security.hypervisor" "${out}/helper-entitlements.xml" \
    || fail "helper lost com.apple.security.hypervisor"
log "hypervisor entitlement present on app and helper"

log "spctl -a -vvv --type execute"
if spctl -a -vvv --type execute "$app"; then
    log "spctl accepted"
else
    if [[ "$skip_notarize" -eq 1 ]]; then
        log "spctl rejected (expected: notarization was skipped; Gatekeeper needs Developer ID + notarytool + staple)"
    else
        fail "spctl -a -vvv rejected the signed bundle"
    fi
fi

zip="${out}/Ternvale.zip"
rm -f "$zip"
log "zip ${zip}"
ditto -c -k --keepParent "$app" "$zip"

verify_flags=(release-verify --app "$app")
if [[ "$skip_notarize" -eq 0 ]]; then
    log "notarytool submit (team=${team:-unset} key-id=${key_id})"
    xcrun notarytool submit "$zip" \
        --key "$key_path" \
        --key-id "$key_id" \
        --issuer "$issuer" \
        --wait
    log "stapler staple"
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
    ditto -c -k --keepParent "$app" "$zip"
    log "re-check entitlements after staple"
    codesign --display --entitlements :- "$embedded" 2>/dev/null \
        | tee "${out}/helper-entitlements-stapled.xml" >&2
    grep -q "com.apple.security.hypervisor" "${out}/helper-entitlements-stapled.xml" \
        || fail "helper lost com.apple.security.hypervisor after staple"
    log "notarized app: $app"
    verify_flags+=(--expect-gatekeeper)
else
    log "notarize+staple skipped; signed app: $app zip: $zip"
fi

log "ternvale release-verify"
"$helper" "${verify_flags[@]}"
log "done"
