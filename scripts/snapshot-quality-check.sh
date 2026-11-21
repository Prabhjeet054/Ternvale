#!/usr/bin/env bash
# Step 116 analogs: 8 GiB sparse restore, 5-snapshot counter, blk checksum, corrupt link, dirty audit.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
cargo test -p ternvale-snapshot --lib -- --test-threads=1 --nocapture \
  five_incremental_snapshots_restore_the_counter \
  corrupt_link_is_named_in_the_error \
  eight_gib_sparse_lazy_restore_is_faster_than_full_copy_analog
cargo test -p ternvale-vmm --lib -- --test-threads=1 --nocapture \
  five_snapshots_restore_step53_counter \
  blk_dd_during_snapshot_keeps_disk_and_memory_checksums \
  eight_gib_lazy_mmap_vs_working_set_copy \
  scratch_untracked_write_path_is_caught \
  write_helpers_call_mark_dirty
echo "snapshot-quality-check: ok"
