#!/bin/bash
# Miri on pure-logic crates: config, ACPI, virtqueue (fake RAM), agent protocol,
# ESR decode, network policy.
#
# Uses the fuzz-campaign nightly toolchain when present.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

if [[ -d "$root/.fuzz-toolchain/cargo/bin" ]]; then
    export RUSTUP_HOME="${RUSTUP_HOME:-$root/.fuzz-toolchain/rustup}"
    export CARGO_HOME="${CARGO_HOME:-$root/.fuzz-toolchain/cargo}"
    export PATH="$CARGO_HOME/bin:$PATH"
fi

if ! cargo +nightly miri --version >/dev/null 2>&1; then
    echo "miri: need rustup nightly with the miri component" >&2
    echo "  rustup +nightly component add miri" >&2
    exit 1
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target/miri}"
export MIRIFLAGS="${MIRIFLAGS:--Zmiri-disable-isolation}"

run() {
    echo "miri: $*"
    cargo +nightly miri test \
        --config "target.aarch64-apple-darwin.runner = '${root}/scripts/miri-run.sh'" \
        "$@"
}

run -p ternvale-config --lib
# iasl tests spawn the host compiler; Miri cannot posix_spawn on macOS.
run -p ternvale-acpi --lib -- --skip iasl
run -p ternvale-agent-proto --lib
run -p ternvale-net --lib policy
run -p ternvale-vmm --lib esr
# `queue` matches virtio-blk multi-queue (fcntl). Tests live in
# `virtio::queue::tests` (`queue_tests.rs`).
run -p ternvale-devices --lib 'virtio::queue::tests' -- --skip ten_thousand

echo "miri: ok"
