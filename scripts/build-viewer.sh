#!/usr/bin/env bash
# Build the Step 44 dev viewer (tools/ternvale-viewer/main.swift) into
# <target>/viewer/ternvale-viewer, where `ternvale run --display window` finds it.
# Skips the build when the binary is newer than the source; --force rebuilds.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="$root/tools/ternvale-viewer/main.swift"
out_dir="${CARGO_TARGET_DIR:-$root/target}/viewer"
out="$out_dir/ternvale-viewer"

if ! command -v swiftc >/dev/null 2>&1; then
    echo "build-viewer: swiftc not found; install the Xcode command line tools (xcode-select --install)" >&2
    exit 1
fi
if [[ "${1:-}" != "--force" && -x "$out" && "$out" -nt "$src" ]]; then
    echo "build-viewer: $out is up to date" >&2
    exit 0
fi
mkdir -p "$out_dir"
swiftc -O -swift-version 5 -target arm64-apple-macos14.0 -framework AppKit -o "$out" "$src"
echo "build-viewer: built $out" >&2
