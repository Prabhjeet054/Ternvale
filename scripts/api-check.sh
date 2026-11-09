#!/bin/bash
# Step 83 test: goldens, scratch field rename, UID reject, v1 optional fields, lifecycle.
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-${root}/entitlements/ternvale-hv-only.entitlements}"

log() { printf 'api-check: %s\n' "$*" >&2; }

hs="$root/ternvale-api/src/handshake.rs"
backup=$(mktemp)
cp "$hs" "$backup"
branch="tv-api-golden-scratch-$$"
prev=$(git branch --show-current || true)
restore() {
  cp "$backup" "$hs"
  rm -f "$backup"
  if git rev-parse --verify "$branch" >/dev/null 2>&1; then
    if [[ -n "$prev" ]]; then
      git checkout -q "$prev" || true
    fi
    git branch -D "$branch" >/dev/null 2>&1 || true
  fi
}
trap restore EXIT

log "golden-file tests (must pass)"
cargo test --offline -p ternvale-api --test golden -- --test-threads=1
cargo test --offline -p ternvale-cli --lib -- --test-threads=1 golden_files_freeze_legacy_cmd_and_error_shapes

log "scratch branch ${branch}: rename HandshakeResult.api_version; goldens must fail"
git checkout -b "$branch"
python3 - "$hs" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text()
old = """pub struct HandshakeResult {
    pub ok: bool,
    pub api_version: u32,"""
new = """pub struct HandshakeResult {
    pub ok: bool,
    pub protocol_version: u32,"""
if old not in text:
    raise SystemExit("handshake.rs did not contain HandshakeResult.api_version")
text = text.replace(old, new, 1)
text = text.replace("api_version: requested,", "protocol_version: requested,", 1)
path.write_text(text)
PY
set +e
cargo test --offline -p ternvale-api --test golden handshake_request_and_result_match_goldens -- --test-threads=1
scratch_status=$?
set -e
cp "$backup" "$hs"
if [[ "$scratch_status" -eq 0 ]]; then
  log "ERROR: goldens still passed after renaming HandshakeResult.api_version"
  exit 1
fi
log "goldens failed after field rename (exit ${scratch_status}) as required"
if [[ -n "$prev" ]]; then
  git checkout -q "$prev"
fi
git branch -D "$branch"
trap - EXIT
rm -f "$backup"

log "peer UID reject + v1 client ignores extra handshake fields + lifecycle"
cargo test --offline -p ternvale-cli --lib -- --test-threads=1 simulated_wrong_uid_is_rejected
cargo test --offline -p ternvale-api --lib -- --test-threads=1 v1_client_ignores_new_optional_server_fields
cargo test --offline -p ternvale-client --lib -- --test-threads=1 create_start_agent_snapshot_stop

log "ok"
