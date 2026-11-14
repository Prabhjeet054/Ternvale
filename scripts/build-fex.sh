#!/bin/sh
# Build FEX-Emu for aarch64 Linux guests. This is not a macOS/host binary.
# Rosetta is unavailable on the native Hypervisor.framework backend; do not
# extract Apple's translator. FEX license: MIT (see THIRD_PARTY_NOTICES.md).
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
# Pin a release tag; bump with a PROGRESS.md note after reading FEX LICENSE.
# FEX-2611.1 is current as of 2026-11 (MIT). FEXInterpreter may be named `FEX`.
FEX_TAG="${FEX_TAG:-FEX-2611.1}"
src="${FEX_SRC:-$root/target/fex/src}"
prefix="${FEX_PREFIX:-$root/target/fex/prefix}"

log() { printf 'build-fex: %s\n' "$*" >&2; }

uname_m=$(uname -m)
uname_s=$(uname -s)
if [ "$uname_s" != "Linux" ] || [ "$uname_m" != "aarch64" ]; then
  if command -v docker >/dev/null 2>&1; then
    log "host is $uname_s/$uname_m; using docker linux/arm64"
    exec docker run --rm --platform linux/arm64 \
      -v "$root:$root" -w "$root" \
      -e FEX_TAG="$FEX_TAG" -e FEX_SRC="$src" -e FEX_PREFIX="$prefix" \
      debian:bookworm-slim \
      sh -c 'apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq git cmake ninja-build clang g++ python3 pkg-config libssl-dev lld >/dev/null && exec sh scripts/build-fex.sh'
  fi
  log "need aarch64 Linux (or docker) to compile FEX; refusing macOS/host build"
  exit 1
fi

if [ ! -d "$src/.git" ]; then
  mkdir -p "$(dirname "$src")"
  git clone --depth 1 --recurse-submodules --shallow-submodules --branch "$FEX_TAG" \
    https://github.com/FEX-Emu/FEX.git "$src"
fi

cmake -S "$src" -B "$src/build" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX="$prefix" \
  -DBUILD_TESTING=OFF \
  -DENABLE_LTO=OFF
cmake --build "$src/build"
cmake --install "$src/build"
log "installed to $prefix (copy with scripts/pack-linux-guest-tools.sh)"
