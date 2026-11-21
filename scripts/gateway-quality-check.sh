#!/usr/bin/env bash
# Step 118 analogs: curl-like HTTPS against ternvaled, tokens, bind, rate limit,
# serial WS, PNG, JSON fuzz (wall clock: TERNVALE_GATEWAY_FUZZ_SECS, default 3;
# set 600 for a 10-minute live fuzz).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
export TERNVALE_GATEWAY_FUZZ_SECS="${TERNVALE_GATEWAY_FUZZ_SECS:-3}"
cargo test -p ternvale-gateway --lib -- --test-threads=1 --nocapture \
  curl_create_start_snapshot_and_readonly_cannot_stop \
  wrong_client_cert_and_expired_token_are_rejected_and_audited \
  default_bind_is_loopback_lsof_analog \
  hammer_triggers_rate_limit \
  serial_websocket_and_valid_png_screenshot \
  fuzz_json_endpoints_no_panic
echo "gateway-quality-check: ok (fuzz ${TERNVALE_GATEWAY_FUZZ_SECS}s)"
