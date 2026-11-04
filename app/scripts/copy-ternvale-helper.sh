#!/bin/bash
# Copy and ad-hoc sign the `ternvale` CLI and `ternvale-vmnetd` into the app bundle.
# Usage: copy-ternvale-helper.sh <path-to-.app-bundle>
set -euo pipefail

bundle="${1:?bundle path required}"
dest="${bundle}/Contents/Helpers"
macos_dir="${bundle}/Contents/MacOS"
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/../.." && pwd) # repo root (Ternvale/)
entitlements="${root}/entitlements/ternvale.entitlements"
vmnet_entitlements="${root}/entitlements/ternvale-vmnetd.entitlements"
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
        cargo build --release --target aarch64-apple-darwin -p ternvale-cli --features vmnet --manifest-path "${root}/Cargo.toml" --quiet
        bin="${root}/target/aarch64-apple-darwin/release/ternvale"
    else
        cargo build -p ternvale-cli --features vmnet --manifest-path "${root}/Cargo.toml" --quiet
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

if [[ "${CONFIGURATION:-}" == "Release" ]]; then
    cargo build --release --target aarch64-apple-darwin -p ternvale-vmnetd --manifest-path "${root}/Cargo.toml" --quiet
    vmnetd="${root}/target/aarch64-apple-darwin/release/ternvale-vmnetd"
    if [[ ! -x "${vmnetd}" ]]; then
        vmnetd="${root}/target/release/ternvale-vmnetd"
    fi
else
    cargo build -p ternvale-vmnetd --manifest-path "${root}/Cargo.toml" --quiet
    vmnetd="${root}/target/debug/ternvale-vmnetd"
fi
mkdir -p "${macos_dir}"
cp -f "${vmnetd}" "${macos_dir}/ternvale-vmnetd"
chmod +x "${macos_dir}/ternvale-vmnetd"
if [[ -f "${vmnet_entitlements}" ]]; then
    codesign -s - --entitlements "${vmnet_entitlements}" -f "${macos_dir}/ternvale-vmnetd" >/dev/null 2>&1 \
        || codesign -s - -f "${macos_dir}/ternvale-vmnetd"
fi
echo "copy-ternvale-helper: installed ${macos_dir}/ternvale-vmnetd" >&2
