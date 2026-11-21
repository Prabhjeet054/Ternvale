#!/usr/bin/env bash
# Step 117 analogs: sysrq panic policies, SBSA watchdog, SIGKILL supervisor, dump privacy.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
cargo test -p ternvale-vmm --lib -- --test-threads=1 --nocapture \
  sysrq_panic_applies_each_on_panic_policy_including_the_dump \
  watchdog_without_refresh_applies_each_on_watchdog_policy \
  dump_file_is_capped_and_privacy_warning_names_secrets
cargo test -p ternvale-cli --lib -- --test-threads=1 --nocapture \
  kill_dash_nine_of_vmm_child_restarts_with_unclean_shutdown_marker \
  three_quick_vmm_crashes_stop_restarts_with_a_notification \
  crash_report_includes_capped_dump_and_privacy_warning
echo "reliability-quality-check: ok"
