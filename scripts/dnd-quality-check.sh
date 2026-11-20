#!/usr/bin/env bash
# Step 114 analogs: PNG/RTF paste, 50 MiB checksums, cancel cleanup, CON/.. names, 1000 files.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
cargo test -p ternvale-cli --lib -- --test-threads=1 --nocapture \
  preview_png_negotiates_over_text_and_survives_paste_into_paint \
  pages_rtf_and_html_formatting_survives_wordpad \
  finder_50_mib_round_trip_checksums_match \
  cancel_mid_transfer_cleans_stage \
  guest_traversal_and_con_stay_contained_with_log \
  folder_of_one_thousand_files_completes_with_count
echo "dnd-quality-check: ok"
