# Minimized libFuzzer artifacts

When `make fuzz-long` (or `scripts/fuzz-campaign.sh`) finds a crash, timeout,
or OOM, put the minimized input here as `tests/regressions/<target>/<name>`
and replay it from `regression_inputs_do_not_panic` in `ternvale-fuzz`.

The Step 79 test campaign (2026-11-09, 10 minutes × 18 targets, ~24.7 million
executions) produced **no** crash, timeout, or OOM artifacts.
