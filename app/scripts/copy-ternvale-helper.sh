#!/bin/bash
# Copy and ad-hoc sign the `ternvale` CLI into the app bundle Helpers folder.
# Usage: copy-ternvale-helper.sh <path-to-.app-bundle>
set -euo pipefail

bundle="${1:?bundle path required}"
dest="${bundle}/Contents/Helpers"
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/../.." && pwd) # repo root (Ternvale/)
entitlements="${root}/entitlements/ternvale.entitlements"
if [[ -n "${TERNVALE_HELPER_SRC:-}" ]]; then
    bin="${TERNVALE_HELPER_SRC}"
elif [[ "${CONFIGURATION:-}" == "Release" ]]; then
    bin="${root}/target/aarch64-apple-darwin/release/ternvale"
    if [[ ! -x "${bin}" ]]; then
        bin="${root}/target/release/ternvale"
    fi
else
    bin="${root}/target/debug/ternvale"
fi

if [[ ! -x "${bin}" ]]; then
    echo "copy-ternvale-helper: building ${bin}" >&2
    if [[ "${CONFIGURATION:-}" == "Release" ]]; then
        cargo build --release --target aarch64-apple-darwin -p ternvale-cli --manifest-path "${root}/Cargo.toml" --quiet
        bin="${root}/target/aarch64-apple-darwin/release/ternvale"
    else
        cargo build -p ternvale-cli --manifest-path "${root}/Cargo.toml" --quiet
        bin="${root}/target/debug/ternvale"
    fi
fi

mkdir -p "${dest}"
cp -f "${bin}" "${dest}/ternvale"
chmod +x "${dest}/ternvale"
if [[ -f "${entitlements}" ]]; then
    codesign -s - --entitlements "${entitlements}" -f "${dest}/ternvale" >/dev/null 2>&1 \
        || codesign -s - -f "${dest}/ternvale"
fi
echo "copy-ternvale-helper: installed ${dest}/ternvale" >&2
