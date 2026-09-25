#!/bin/bash
# Boot the test kernel and match serial output. Exit 0 only on a clean SYSTEM_OFF.
# Logs and the guest serial transcript land in target/boot-logs/<timestamp>/.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

stamp=$(date +%Y%m%d-%H%M%S)
log_dir="${root}/target/boot-logs/${stamp}"
mkdir -p "$log_dir"
export TERNVALE_BOOT_LOG_DIR="$log_dir"

printf 'boot-test: logs %s\n' "$log_dir"
set +e
cargo test -p ternvale-devices --features boot-test --test boot -- --nocapture
status=$?
set -e
printf 'boot-test: exit %s\n' "$status"
printf 'boot-test: artifacts %s\n' "$log_dir"
exit "$status"
