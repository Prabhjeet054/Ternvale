#!/usr/bin/env bash
# Nightly compatibility matrix. Step 88 adds scripts/compat and `make compat`;
# until then this is an explicit skip, not a silent pass-as-success of guests.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "${root}"
log() { printf 'compat-nightly: %s\n' "$*" >&2; }

if make -n compat >/dev/null 2>&1; then
    log "running make compat"
    make compat
    exit $?
fi

log "scripts/compat / make compat not in tree yet (Step 88); skipping matrix"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    echo "Compatibility matrix skipped: \`make compat\` lands in Step 88." >>"${GITHUB_STEP_SUMMARY}"
fi
exit 0
