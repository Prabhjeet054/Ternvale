#!/usr/bin/env bash
# Confirm the repo and built binaries do not contain a license signing seed.
# Usage: scripts/license-secret-scan.sh [seed-file-to-prove-absent]
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

fail() { echo "license-secret-scan: $*" >&2; exit 1; }
ok() { echo "license-secret-scan: $*"; }

if git ls-files --error-unmatch 'secrets/*' >/dev/null 2>&1; then
  fail "tracked files under secrets/ (signing seeds must not be committed)"
fi
tracked_seeds="$(git ls-files '*.seed' '**/*.seed' '**/license.seed' || true)"
if [[ -n "${tracked_seeds}" ]]; then
  fail "tracked seed files: ${tracked_seeds}"
fi

# Sparkle appcast tooling builds a PEM wrapper from a seed file; that is not a
# committed Ed25519 license seed. Exclude this scanner and that generator.
if git grep -nF -- 'BEGIN PRIVATE KEY' \
  -- ':!scripts/license-secret-scan.sh' \
  -- ':!scripts/updates/generate_appcast.py' \
  -- ':!drivers/release/secrets_grep.py' >/dev/null; then
  fail "PEM private key material in the tree"
fi

# Production verifier is public-only (rg sees untracked files; git grep does not).
if ! grep -n -- 'EMBEDDED_PUBLIC_HEX' ternvale-license/src/pubkey.rs >/dev/null; then
  fail "missing embedded public key"
fi
hits="$(grep -R -n --include='*.rs' --exclude-dir=target --exclude-dir=.git \
  -- 'SigningKey::from_bytes' . || true)"
if echo "$hits" | grep -v 'ternvale-license/' | grep -q .; then
  fail "SigningKey::from_bytes outside ternvale-license:
$(echo "$hits" | grep -v 'ternvale-license/')"
fi

bins=()
while IFS= read -r p; do
  bins+=("$p")
done < <(find "${CARGO_TARGET_DIR:-target}" -type f \( -name ternvale -o -name ternvale-license-sign \) 2>/dev/null | head -n 20)

if [[ ${#bins[@]} -eq 0 ]]; then
  echo "license-secret-scan: no built binaries yet (ok if called before cargo build)" >&2
else
  for b in "${bins[@]}"; do
    if strings "$b" | grep -E 'BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY' >/dev/null; then
      fail "private key PEM in $(basename "$b")"
    fi
  done
  ok "no PEM private keys in ${#bins[@]} binary(ies)"
fi

if [[ $# -ge 1 && -f "$1" ]]; then
  seed="$1"
  if [[ "$(wc -c < "$seed" | tr -d ' ')" != "32" ]]; then
    fail "proof seed $seed is not 32 bytes"
  fi
  hex="$(xxd -p -c 32 "$seed" | tr -d '\n')"
  if grep -R -F --include='*.rs' --include='*.md' --include='*.toml' --include='*.sh' \
    --include='*.json' --include='*.txt' --exclude-dir=target --exclude-dir=.git \
    -- "$hex" . >/dev/null; then
    fail "proof seed hex is in the tree"
  fi
  for b in "${bins[@]+"${bins[@]}"}"; do
    if strings "$b" | grep -F "$hex" >/dev/null; then
      fail "proof seed hex is in $(basename "$b")"
    fi
  done
  ok "proof seed is absent from repo and binaries"
fi

ok "no committed license private key material"
