#!/bin/bash
# Local RC zip always; git tag only on a clean tree when requested.
# Gatekeeper/notarize stay blocked until Developer ID Application exists.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tag="${TERNVALE_RC_TAG:-v0.1.0-rc1}"
do_zip=1
do_git=0

log() { printf 'rc-tag: %s\n' "$*" >&2; }
fail() { log "error: $*"; exit 1; }

usage() {
    cat <<'EOF'
usage: scripts/rc-tag.sh [--zip] [--git] [--no-zip]

  --zip     Build dist/Ternvale.zip via release.sh --skip-notarize (default)
  --no-zip  Skip the zip
  --git     Create annotated tag TERNVALE_RC_TAG (default v0.1.0-rc1)
            Requires a clean working tree. Does not push.
            Refuses unless Developer ID is present or
            TERNVALE_RC_TAG_WITHOUT_NOTARY=1 (unsigned/dev-signed RC only).
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --zip) do_zip=1; shift ;;
        --no-zip) do_zip=0; shift ;;
        --git) do_git=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown argument: $1" ;;
    esac
done

status_file="${root}/dist/RC-STATUS.txt"
mkdir -p "${root}/dist"

dev_id=0
if "${root}/scripts/require-developer-id.sh" >/dev/null 2>&1; then
    dev_id=1
fi

if [[ "$do_zip" -eq 1 ]]; then
    log "building signed zip (notarize skipped unless Developer ID + notary env)"
    if [[ "$dev_id" -eq 1 && -n "${TERNVALE_NOTARY_KEY_PATH:-}" ]]; then
        "${root}/scripts/release.sh"
    else
        "${root}/scripts/release.sh" --skip-notarize
    fi
fi

{
    echo "tag=${tag}"
    echo "date=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "developer_id=${dev_id}"
    if [[ -f "${root}/dist/Ternvale.zip" ]]; then
        echo "zip=${root}/dist/Ternvale.zip"
        echo "zip_bytes=$(wc -c < "${root}/dist/Ternvale.zip" | tr -d ' ')"
    else
        echo "zip=missing"
    fi
    if [[ "$dev_id" -eq 0 ]]; then
        echo "gatekeeper=rejected (no Developer ID Application in Keychain)"
        echo "notarize=skipped"
        echo "spctl=will fail until Developer ID + notarytool + staple"
    else
        echo "gatekeeper=Developer ID present; run spctl after staple"
    fi
    echo "git_tag=not created (use --git on a clean tree)"
} >"$status_file"
log "wrote ${status_file}"

if [[ "$do_git" -eq 0 ]]; then
    log "skipping git tag (pass --git)"
    cat "$status_file" >&2
    exit 0
fi

if [[ "$dev_id" -eq 0 && "${TERNVALE_RC_TAG_WITHOUT_NOTARY:-}" != "1" ]]; then
    fail "no Developer ID Application; set TERNVALE_RC_TAG_WITHOUT_NOTARY=1 to tag a non-Gatekeeper RC, or create the cert (scripts/require-developer-id.sh)"
fi

cd "$root"
if [[ -n "$(git status --porcelain)" ]]; then
    fail "working tree is dirty; commit the HV/fuzz/signing fixes before tagging ${tag}"
fi
if git rev-parse "$tag" >/dev/null 2>&1; then
    log "tag ${tag} already exists at $(git rev-parse "$tag")"
    exit 0
fi
git tag -a "$tag" -m "Ternvale ${tag}"
log "created ${tag} at $(git rev-parse HEAD)"
log "not pushing (git push origin ${tag} when the zip is notarized)"
sed -i.bak "s/^git_tag=.*/git_tag=${tag}/" "$status_file" && rm -f "${status_file}.bak"
