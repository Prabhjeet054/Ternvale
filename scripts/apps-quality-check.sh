#!/usr/bin/env bash
# Step 113 analogs: Launchpad start-while-off, Open With, URL gates, uninstall.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
cargo test -p ternvale-cli --lib -- --test-threads=1 --nocapture \
  launchpad_stub_starts_stopped_vm_and_launches_notepad \
  finder_open_with_txt_maps_share_path_to_guest_notepad \
  guest_https_opens_on_host_browser_analog \
  file_and_unknown_scheme_are_blocked_with_reason \
  open_on_host_off_keeps_links_in_the_guest \
  uninstall_leaves_no_ternvale_stubs
echo "apps-quality-check: ok"
