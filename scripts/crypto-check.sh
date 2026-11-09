#!/bin/bash
# Step 82: marker grep, wrong key, log redaction, GCM tamper, XTS microbench.
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-${root}/entitlements/ternvale-hv-only.entitlements}"

log() { printf 'crypto-check: %s\n' "$*" >&2; }

log "unit tests (marker grep, wrong key, GCM tamper, log redaction, XTS bench)"
cargo test --offline -p ternvale-crypto -p ternvale-snapshot -p ternvale-devices -p ternvale-cli --lib \
  -- --test-threads=1 \
  rest_tests xts_hides plaintext_control wrong_key xts_fio encrypted_delta set_data_key_debug gcm_bundle_hides apply_rejects

if [[ -f "${root}/bench-results/encryption-xts-nohv.tsv" ]]; then
  log "XTS microbench:"
  cat "${root}/bench-results/encryption-xts-nohv.tsv" >&2
fi

if [[ "${TERNVALE_CRYPTO_BOOT:-0}" == "1" ]]; then
  log "hypervisor encryption scenario"
  "${script_dir}/encryption-boot-check.sh"
fi

log "ok"
