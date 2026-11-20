#!/usr/bin/env bash
# Step 111 privacy/capture analogs: arecord jack, 720p, deny UI, toggle-off, log grep.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
cargo test -p ternvale-audio-host -- --test-threads=1 --nocapture \
  tone_capture_has_levels hd720_preview
cargo test -p ternvale-devices -- --test-threads=1 --nocapture \
  arecord_sees_a_capture_stream windows_sound_recorder denied_or_toggled_off
cargo test -p ternvale-agent -- --test-threads=1 --nocapture \
  assembles_720p_preview
cargo test -p ternvale-cli --lib -- --test-threads=1 --nocapture \
  system_settings_deny toggles_off_stop_capturing host_logs_and_temp
echo "privacy-capture-check: ok"
