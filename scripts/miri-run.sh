#!/bin/bash
# Workspace `.cargo/config.toml` uses sign-and-run.sh. cargo-miri needs its
# own runner so the interpreter, not a Mach-O, is exec'd.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
if [[ -d "$root/.fuzz-toolchain/cargo/bin" ]]; then
    export RUSTUP_HOME="${RUSTUP_HOME:-$root/.fuzz-toolchain/rustup}"
    export CARGO_HOME="${CARGO_HOME:-$root/.fuzz-toolchain/cargo}"
    export PATH="$CARGO_HOME/bin:$PATH"
fi
exec cargo-miri runner "$@"
