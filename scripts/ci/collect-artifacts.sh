#!/usr/bin/env bash
# Copy recent host logs, guest serial transcripts, boot/regression dirs, and
# crash bundles into a directory for actions/upload-artifact.
# Does not walk `target/` (cargo build trees); only known log directories.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
dest="${1:-${root}/ci-artifacts}"
# Files older than this (days) or larger than this (KiB) are skipped.
max_age_days="${TERNVALE_CI_LOG_DAYS:-2}"
max_kib="${TERNVALE_CI_LOG_MAX_KIB:-4096}"
max_total_mib="${TERNVALE_CI_LOG_TOTAL_MIB:-256}"
log() { printf 'collect-artifacts: %s\n' "$*" >&2; }

mkdir -p "${dest}"
copied=0
bytes=0
budget=$((max_total_mib * 1024 * 1024))

copy_recent() {
    local src=$1 name=$2
    [[ -d "${src}" ]] || return 0
    mkdir -p "${dest}/${name}"
    local n=0
    while IFS= read -r -d '' file; do
        if [[ "${bytes}" -ge "${budget}" ]]; then
            log "stopping at ${max_total_mib} MiB budget"
            break
        fi
        local rel=${file#"${src}"/}
        mkdir -p "${dest}/${name}/$(dirname "${rel}")"
        if cp "${file}" "${dest}/${name}/${rel}" 2>/dev/null; then
            local sz
            sz=$(stat -f%z "${file}" 2>/dev/null || echo 0)
            bytes=$((bytes + sz))
            n=$((n + 1))
            copied=$((copied + 1))
        fi
    done < <(
        find "${src}" -type f \
            -mtime "-${max_age_days}" \
            -size "-${max_kib}k" \
            \( -name '*.log' -o -name '*.txt' -o -name '*.md' -o -name '*.json' \
               -o -name '*.zip' -o -name '*.dat' -o -name '*serial*' -o -name '*report*' \) \
            -print0 2>/dev/null || true
    )
    log "copied ${n} files from ${src} -> ${name}"
}

copy_recent "${HOME}/Library/Logs/Ternvale" host-logs
copy_recent "${root}/target/control-logs" control-logs
copy_recent "${root}/target/regression" regression
copy_recent "${root}/target/boot-logs" boot-logs
copy_recent "${root}/target/acpi-check" acpi-check
copy_recent "${root}/target/bench-logs" bench-logs
copy_recent "${root}/bench-results" bench-results
copy_recent "${root}/ternvale-fuzz/fuzz/artifacts" fuzz-artifacts

log "files copied=${copied} dest=${dest}"
exit 0
