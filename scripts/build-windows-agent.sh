#!/bin/bash
# Cross-compile ternvale-agent for Windows ARM64 from macOS (cargo-xwin).
#
# Cross-compile from macOS. Live-tested 2026-11-04 with:
#   rustup 1.29.3, rustc 1.99.1, cargo-xwin 0.23.1, Apple clang 21.0.0.
# TODO(verify): re-pin if xwin's MSVC CRT package changes.
#
# Usage:
#   ./scripts/build-windows-agent.sh [OUT_FILE]
#     default: target/aarch64-pc-windows-msvc/release/ternvale-agent.exe
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
target="aarch64-pc-windows-msvc"
target_dir="${CARGO_TARGET_DIR:-${root}/target}"
built="${target_dir}/${target}/release/ternvale-agent.exe"
default_out="${root}/target/guest/ternvale-agent.exe"
out="${1:-${default_out}}"

log() {
    printf 'build-windows-agent: %s\n' "$*" >&2
}

if ! command -v cargo >/dev/null 2>&1; then
    log "missing cargo"
    exit 1
fi
if ! command -v rustup >/dev/null 2>&1; then
    log "missing rustup (needed for ${target})"
    exit 1
fi

if ! rustup target list --installed | grep -qx "${target}"; then
    log "adding rustup target ${target}"
    rustup target add "${target}"
fi

if ! cargo xwin --version >/dev/null 2>&1; then
    log "cargo-xwin is not installed. TODO(verify) version before pinning."
    log "install with: cargo install cargo-xwin --locked"
    exit 1
fi

log "cargo xwin build --release -p ternvale-agent --target ${target}"
(
    cd "${root}"
    cargo xwin build --locked --release -p ternvale-agent --target "${target}"
)

if [ ! -f "${built}" ]; then
    log "missing ${built}"
    exit 1
fi
if [ "${out}" != "${built}" ]; then
    mkdir -p "$(dirname "${out}")"
    cp "${built}" "${out}"
fi
log "wrote ${out} ($(wc -c <"${out}" | tr -d ' ') bytes)"
log "on the guest: ternvale-agent.exe --install && sc start TernvaleGuestAgent"
