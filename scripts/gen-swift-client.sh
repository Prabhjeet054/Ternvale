#!/bin/sh
# Write the Swift JSON-RPC client from ternvale-api (Step 83).
set -eu
cd "$(dirname "$0")/.."
cargo run -q -p ternvale-api --bin gen-swift-client
