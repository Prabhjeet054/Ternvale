#!/usr/bin/env bash
# Step 119 quality: sign-tool licenses, unit/quality tests, secret scan, parser fuzz.
# Fuzz wall clock: TERNVALE_LICENSE_FUZZ_SECS (default 600 = 10 minutes; 3 for CI analog).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

fuzz_secs="${TERNVALE_LICENSE_FUZZ_SECS:-600}"
work="$(mktemp -d "${TMPDIR:-/tmp}/tvlk-quality.XXXXXX")"
trap 'rm -rf "$work"' EXIT
seed="$work/license.seed"

echo "license-quality-check: sign tool keypair in $work"
cargo run -q -p ternvale-license --bin ternvale-license-sign -- --gen-seed "$seed"
cargo run -q -p ternvale-license --bin ternvale-license-sign -- --key "$seed" --print-public >/dev/null
cargo run -q -p ternvale-license --bin ternvale-license-sign -- \
  --key "$seed" --edition pro --seats 1 --days 365 --id LIC-Q --out "$work/valid.tvlk"
cargo run -q -p ternvale-license --bin ternvale-license-sign -- \
  --key "$seed" --edition personal --seats 1 --expiry-unix 1000 --id LIC-OLD --out "$work/expired.tvlk"
test -s "$work/valid.tvlk"
test -s "$work/expired.tvlk"

echo "license-quality-check: unit + sign-tool + export/start"
cargo test -p ternvale-license -- --test-threads=1
cargo test -p ternvale-cli --lib expired_export_succeeds_start_refused -- --test-threads=1

echo "license-quality-check: secret scan"
cargo build -q -p ternvale-license --bin ternvale-license-sign
cargo build -q -p ternvale-cli --bin ternvale
./scripts/license-secret-scan.sh "$seed"

echo "license-quality-check: parser fuzz ${fuzz_secs}s"
TERNVALE_LICENSE_FUZZ_SECS="$fuzz_secs" cargo test -p ternvale-license \
  fuzz_parser_ten_minutes -- --ignored --test-threads=1 --nocapture

echo "license-quality-check: ok (fuzz ${fuzz_secs}s)"
