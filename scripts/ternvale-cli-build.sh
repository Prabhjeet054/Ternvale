#!/usr/bin/env bash
# Build ternvale-cli with optional Cargo features (space-separated).
#   TERNVALE_CLI_FEATURES="gpu-3d" ./scripts/ternvale-cli-build.sh
#   ./scripts/ternvale-cli-build.sh --quiet
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
features="${TERNVALE_CLI_FEATURES:-}"
args=(build -p ternvale-cli)
[[ "${1:-}" == --quiet ]] && args+=(-q)
if [[ -n "$features" ]]; then
    args+=(--features "$features")
fi
(cd "$root" && cargo "${args[@]}")
