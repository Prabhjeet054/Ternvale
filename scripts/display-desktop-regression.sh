#!/usr/bin/env bash
# Step 65 regression: desktop stress with the default (2D-only) ternvale-cli build.
# Confirms TERNVALE_CLI_FEATURES is unset so gpu3d stays off.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
unset TERNVALE_CLI_FEATURES
export TERNVALE_CLI_FEATURES=""
log() { printf 'display-desktop-regression: %s\n' "$*" >&2; }
log "building ternvale-cli without gpu-3d"
"$root/scripts/ternvale-cli-build.sh" --quiet
log "running display-desktop-stress.sh (Step 65)"
exec "$root/scripts/display-desktop-stress.sh"
