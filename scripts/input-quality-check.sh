#!/usr/bin/env bash
# Step 110 input quality analogs: overlay, capture, layouts, key-up, gamepad.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
./scripts/build-viewer.sh
cargo test -p ternvale-gpu -- --test-threads=1 --nocapture \
  ibeam_overlay cursor_overlay_is_sent_while capture_hint_relative
cargo test -p ternvale-devices -- --test-threads=1 --nocapture \
  azerty_and_devanagari held_key_release gamepad_events_match_evtest
cargo test -p ternvale-cli --lib -- --test-threads=1 --nocapture \
  azerty_q_position escape_capture focus_loss_key_up
echo "input-quality-check: ok"
