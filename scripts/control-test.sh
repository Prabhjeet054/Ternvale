#!/bin/bash
# End-to-end test of `ternvale run` and its control socket on a real VM.
#
# Runs the `needs-hv` integration tests in ternvale-cli/tests/control.rs:
#   - status shows running; pause freezes a guest `while true; do date; sleep 1; done`
#     loop (no new lines, vCPU runs unchanged, guest clock does not jump);
#     resume continues; stop exits 0; pause/resume/stop on a stopping or
#     exited VM fail with clear errors; the host log has every transition.
#   - stop --force exits 0.
# Artifacts: target/control-logs/<secs>-<test>/ (transitions.txt, guest-serial.log,
# run.stderr, the test's own log).
set -euo pipefail

cd "$(dirname "$0")/.."
exec cargo test -p ternvale-cli --test control -- --ignored --nocapture --test-threads=1 "$@"
