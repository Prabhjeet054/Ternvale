#!/usr/bin/env bash
# Step 115 analogs: Windows PDF→Preview, CUPS jobs, tunnel refusals, opt-in off.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
cargo test -p ternvale-devices --lib -- --test-threads=1 --nocapture \
  windows_print_to_pdf_opens_preview_analog \
  windows_and_linux_print_to_cups_pdf_analog \
  tunnel_refuses_admin_delete_and_arbitrary_ports_and_logs \
  print_opt_in_off_clears_guest_printers
echo "print-quality-check: ok"
