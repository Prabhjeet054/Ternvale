#!/usr/bin/env bash
# Run workspace clippy once, emit GitHub Actions file annotations, exit clippy's code.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "${root}"
json="${root}/target/clippy-ci.json"
mkdir -p "${root}/target"
log() { printf 'clippy-run: %s\n' "$*" >&2; }

set +e
cargo clippy --workspace --message-format=json -- -D warnings >"${json}"
status=$?
set -e
log "cargo clippy exit=${status}"
export CLIPPY_EXIT="${status}"
python3 "${root}/scripts/ci/clippy_annotate.py" "${json}"
exit "${status}"
