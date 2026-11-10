#!/usr/bin/env bash
# Nightly compatibility matrix (Step 88).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "${root}"
log() { printf 'compat-nightly: %s\n' "$*" >&2; }

log "make compat"
make compat
exit $?
