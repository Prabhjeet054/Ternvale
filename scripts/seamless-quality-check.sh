#!/usr/bin/env bash
# Step 112 seamless analogs: overlapping apps, ops, dialog, leak bound, toggle-off.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
cargo test -p ternvale-devices --lib -- --test-threads=1 --nocapture \
  overlapping_notepad_and_calculator
cargo test -p ternvale-cli --lib -- --test-threads=1 --nocapture \
  notepad_and_calculator host_move_resize typing_follows_focus \
  modal_dialog thirty_minute_poll toggling_seamless_off
echo "seamless-quality-check: ok"
