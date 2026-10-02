#!/bin/bash
# Build the guest agent as a static aarch64-linux-musl binary.
#
# There is no musl cross toolchain on the macOS host, so the build runs in a
# linux/arm64 rust:alpine container (native aarch64-unknown-linux-musl) with
# the workspace mounted read-only. Cargo's registry and target dir live in
# named Docker volumes so rebuilds are incremental. The result is checked to
# be a statically linked aarch64 ELF.
#
# Usage:
#   ./scripts/build-guest-agent.sh [OUT_FILE]     (default target/guest/ternvale-agent)
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/target/guest/ternvale-agent}"
image="${RUST_IMAGE:-rust:1-alpine}"

log() {
    printf 'build-guest-agent: %s\n' "$*" >&2
}

command -v docker >/dev/null 2>&1 || { log "missing required tool: docker"; exit 1; }
if ! docker info >/dev/null 2>&1; then
    log "docker is not reachable; start Docker Desktop and retry"
    exit 1
fi

mkdir -p "$(dirname "${out}")"
stage="$(mktemp -d)"
trap 'rm -rf "${stage}"' EXIT

log "building ternvale-agent in ${image} (linux/arm64, static musl)"
docker run --rm --platform linux/arm64 \
    -v "${root}:/src:ro" \
    -v "${stage}:/out" \
    -v ternvale-guest-cargo:/usr/local/cargo/registry \
    -v ternvale-guest-target:/target \
    -e CARGO_TARGET_DIR=/target \
    -e CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static" \
    -e CARGO_PROFILE_RELEASE_STRIP=symbols \
    -w /src \
    "${image}" sh -c '
set -e
apk add --no-cache musl-dev >/dev/null
# Use the image toolchain; the repo rust-toolchain.toml (stable + darwin
# target) would make rustup download another toolchain on every run.
RUSTUP_TOOLCHAIN="$(rustup default | cut -d" " -f1)"
export RUSTUP_TOOLCHAIN
cargo --version
# An explicit --target keeps the static flag off host builds (proc-macros).
cargo build --locked --release --target aarch64-unknown-linux-musl \
    -p ternvale-agent --bin ternvale-agent
cp /target/aarch64-unknown-linux-musl/release/ternvale-agent /out/ternvale-agent
'

info="$(file "${stage}/ternvale-agent")"
log "${info#*: }"
case "${info}" in
    *"ELF 64-bit"*"ARM aarch64"*"statically linked"*) ;;
    *) log "not a static aarch64 ELF; refusing to install it"; exit 1 ;;
esac
cp "${stage}/ternvale-agent" "${out}"
chmod 755 "${out}"
log "wrote ${out} ($(wc -c <"${out}" | tr -d ' ') bytes)"
