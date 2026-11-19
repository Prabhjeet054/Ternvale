#!/usr/bin/env bash
# Step 106 test: infverif analog, attestation layout, secret grep, tools ISO
# outdated flag. Windows WDK infverif / test-sign / pnputil when
# TERNVALE_WINDOWS_GUEST=1.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
stamp="$(date +%Y%m%d-%H%M%S)"
out="$root/target/driver-package/${stamp}"
mkdir -p "$out"
: >"$out/summary.txt"

log() { printf 'driver-package-live: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

log "infverif analog --werror"
python3 "$root/drivers/release/infverif_dch.py" --werror \
  "$root/drivers/tvhello/tvhello.inf" \
  "$root/drivers/tvgpu-dod/tvgpu-dod.inf" \
  | tee "$out/infverif_dch.txt"

log "python packaging tests"
python3 -m unittest discover -s "$root/drivers/release" -p 'test_*.py' -v \
  | tee "$out/python-tests.txt"

log "attestation fixture layout"
stage="$out/attestation-stage"
python3 "$root/drivers/release/attestation_layout.py" --write-fixture "$stage"
python3 "$root/drivers/release/attestation_layout.py" "$stage" \
  | tee "$out/attestation.txt"
echo "attestation_layout=ok" >>"$out/summary.txt"

log "secret grep"
python3 "$root/drivers/release/secrets_grep.py" --root "$root" \
  | tee "$out/secrets.txt"
echo "secrets=none" >>"$out/summary.txt"

log "tools ISO outdated (drivers_version stamp)"
(cd "$root" && cargo test -q -p ternvale-cli --lib drivers_version_flags -- --test-threads=1)
(cd "$root" && cargo test -q -p ternvale-cli --lib build_iso_stages_ternvale_drivers -- --test-threads=1)
echo "tools_iso_outdated=ok" >>"$out/summary.txt"

if [[ "${TERNVALE_WINDOWS_GUEST:-}" == "1" ]]; then
  log "Windows guest: infverif + test-sign + pnputil"
  ssh_opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8)
  guest="${TERNVALE_WINDOWS_SSH:-Administrator@127.0.0.1}"
  port="${TERNVALE_WINDOWS_SSH_PORT:-2222}"
  ssh "${ssh_opts[@]}" -p "${port}" "${guest}" \
    "powershell -File C:\\Ternvale\\drivers\\scripts\\driver-package-live.ps1" \
    | tee "$out/windows-live.txt"
  echo "windows=ran" >>"$out/summary.txt"
else
  log "SKIP WDK infverif/signtool/pnputil: set TERNVALE_WINDOWS_GUEST=1"
  echo "windows=skip_no_guest" >>"$out/summary.txt"
  echo "testsign_off_reject=skip_no_guest (expected HASH_NOT_VERIFIED without attestation)" \
    >>"$out/summary.txt"
fi

log "PASS artifacts $out"
cat "$out/summary.txt" >&2
