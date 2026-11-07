#!/bin/bash
# Fetch the official WinFsp MSI for the Windows tools ISO (unmodified binary
# release). License: GPLv3 + FLOSS exception; see docs/LICENSES.md.
#
# Usage:
#   ./scripts/fetch-winfsp.sh [Library/Caches/Ternvale/winfsp]
#
# Override WINFSP_VER / WINFSP_SHA256 / WINFSP_URL together for another release.
set -euo pipefail

out="${1:-${HOME}/Library/Caches/Ternvale/winfsp}"
ver="${WINFSP_VER:-2.1.25156}"
sha="${WINFSP_SHA256:-073a70e00f77423e34bed98b86e600def93393ba5822204fac57a29324db9f7a}"
name="winfsp-${ver}.msi"
url="${WINFSP_URL:-https://github.com/winfsp/winfsp/releases/download/v2.1/${name}}"

log() {
    printf 'fetch-winfsp: %s\n' "$*" >&2
}

mkdir -p "${out}"
msi="${out}/${name}"
if [[ -f "${msi}" ]] && [[ "$(shasum -a 256 "${msi}" | cut -d' ' -f1)" == "${sha}" ]]; then
    log "already have ${msi}"
else
    log "downloading ${url}"
    curl -fL --retry 3 -o "${msi}.part" "${url}"
    got=$(shasum -a 256 "${msi}.part" | cut -d' ' -f1)
    if [[ "${got}" != "${sha}" ]]; then
        log "sha256 ${got} does not match ${sha}"
        rm -f "${msi}.part"
        exit 1
    fi
    mv "${msi}.part" "${msi}"
fi
ln -sf "${name}" "${out}/winfsp.msi"
log "sha256 ${sha}"
ls -l "${msi}" >&2
