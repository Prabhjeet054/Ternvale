#!/usr/bin/env bash
# Step 109 presentation quality analogs: pacing, dual scanout, HiDPI, 60s idle.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
./scripts/build-viewer.sh
cargo test -p ternvale-gpu -- --test-threads=1 --nocapture \
  static_scanout_sends_no_bytes tearing_bar hidpi_scale second_scanout \
  pack_dirty dpi_follows presenter_uploads
cargo test -p ternvale-gpu -- --ignored --test-threads=1 --nocapture \
  static_desktop_idle_sixty_seconds
echo "present-quality-check: ok"
