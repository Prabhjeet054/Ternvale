#!/usr/bin/env bash
# Print rust=true|false on GITHUB_OUTPUT. Docs-only PRs skip the heavy PR job.
set -euo pipefail

out="${GITHUB_OUTPUT:-/dev/stderr}"
log() { printf 'detect-rust: %s\n' "$*" >&2; }

mark() {
    local value=$1
    log "rust=${value}"
    if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
        echo "rust=${value}" >>"${out}"
    fi
}

if [[ "${GITHUB_EVENT_NAME:-}" != "pull_request" ]]; then
    mark true
    exit 0
fi

base="${GITHUB_BASE_REF:-main}"
if ! git fetch --no-tags --depth=1 origin "${base}" >/dev/null 2>&1; then
    log "could not fetch origin/${base}; assuming rust changes"
    mark true
    exit 0
fi

files=$(git diff --name-only "origin/${base}"...HEAD || true)
if [[ -z "${files}" ]]; then
    mark true
    exit 0
fi
log "changed files:"$'\n'"${files}"

pattern='\.(rs|toml)$|(^|/)Cargo\.lock$|(^|/)Makefile$|(^|/)deny\.toml$|(^|/)\.cargo/|(^|/)rust-toolchain|(^|/)rustfmt\.toml$|(^|/)clippy\.toml$|(^|/)scripts/|(^|/)\.github/|(^|/)ternvale-fuzz/'
if echo "${files}" | grep -E "${pattern}" >/dev/null; then
    mark true
else
    mark false
fi
