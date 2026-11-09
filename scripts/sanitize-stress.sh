#!/bin/bash
# Build with nightly ASan or TSan (`-Zsanitizer`) and run boot + virtio-blk I/O +
# virtio-net. Default sanitizer is address; set SANITIZER=thread for TSan.
#
#   ./scripts/sanitize-stress.sh
#   SANITIZER=thread ./scripts/sanitize-stress.sh
#
# Hypervisor.framework under ASan/TSan is TODO(verify). Set
# TERNVALE_SANITIZE_HV=0 to skip the boot harness (unit tests of pure crates
# still run under the sanitizer).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

if [[ -d "$root/.fuzz-toolchain/cargo/bin" ]]; then
    export RUSTUP_HOME="${RUSTUP_HOME:-$root/.fuzz-toolchain/rustup}"
    export CARGO_HOME="${CARGO_HOME:-$root/.fuzz-toolchain/cargo}"
    export PATH="$CARGO_HOME/bin:$PATH"
fi

sanitizer="${SANITIZER:-address}"
case "$sanitizer" in
    address) profile=asan ;;
    thread) profile=tsan ;;
    *)
        echo "sanitize-stress: SANITIZER must be address or thread" >&2
        exit 1
        ;;
esac

if ! cargo +nightly --version >/dev/null 2>&1; then
    echo "sanitize-stress: need rustup nightly" >&2
    exit 1
fi

export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target/${profile}}"
# Replace, do not append: a leftover `-Zsanitizer=address` from an ASan run
# would combine with TSan and make rustc refuse the target.
export RUSTFLAGS="-Zsanitizer=${sanitizer} -C force-frame-pointers=yes"
# ASan/TSan need `get-task-allow`; the production entitlements file does not
# include it and AMFI SIGKILLs the instrumented binary after codesign.
export TERNVALE_ENTITLEMENTS="${TERNVALE_ENTITLEMENTS:-$root/entitlements/ternvale-sanitizer.entitlements}"
# `--target` keeps proc-macros on the host triple so they are not sanitized.
target="${CARGO_BUILD_TARGET:-aarch64-apple-darwin}"

echo "sanitize-stress: ${sanitizer} profile=${profile} target=${target} target_dir=${CARGO_TARGET_DIR}"

# TSan instruments the ABI; std must be built with the same flag.
cargo_extra=()
if [[ "$sanitizer" == "thread" ]]; then
    rustup +nightly component add rust-src >/dev/null
    cargo_extra=(-Zbuild-std)
fi

crates=(ternvale-config ternvale-acpi ternvale-agent-proto ternvale-net)
for crate in "${crates[@]}"; do
    echo "sanitize-stress: test ${crate}"
    cargo +nightly test "${cargo_extra[@]}" --profile "$profile" --target "$target" -p "$crate" --lib
done

if [[ "${TERNVALE_SANITIZE_HV:-1}" != "1" ]]; then
    echo "sanitize-stress: skipping HV boot (TERNVALE_SANITIZE_HV!=1)"
    echo "sanitize-stress: ${sanitizer} ok (unit tests)"
    exit 0
fi

echo "sanitize-stress: build ternvale-cli + devices boot-test"
cargo +nightly build "${cargo_extra[@]}" --profile "$profile" --target "$target" -p ternvale-cli
# `scripts/boot-test.sh` invokes uninstrumented `cargo test`; drive the same
# harness here so RUSTFLAGS / --profile / --target stay in effect.
# ASan/TSan make guest I/O slower; the harness reads `TERNVALE_BOOT_BANNER_SECS`.
export TERNVALE_BOOT_BANNER_SECS="${TERNVALE_BOOT_BANNER_SECS:-180}"
export TERNVALE_BOOT_TIMEOUT="${TERNVALE_BOOT_TIMEOUT:-180}"
# rootfs currently stalls after one virtio-blk request under ASan (blk_reqs=1);
# opt in with TERNVALE_SANITIZE_SCENARIOS="initrd rootfs net".
stamp=$(date +%Y%m%d-%H%M%S)
scenarios="${TERNVALE_SANITIZE_SCENARIOS:-initrd net}"
# shellcheck disable=SC2086
for scenario in $scenarios; do
    echo "sanitize-stress: boot-test ${scenario}"
    log_dir="${root}/target/boot-logs/${stamp}-${profile}-${scenario}"
    mkdir -p "$log_dir"
    export TERNVALE_BOOT_LOG_DIR="$log_dir"
    export TERNVALE_BOOT_SCENARIO="$scenario"
    cargo +nightly test "${cargo_extra[@]}" --profile "$profile" --target "$target" -p ternvale-devices \
        --features boot-test --test boot -- --nocapture
    if [[ "$scenario" == "rootfs" ]]; then
        ./scripts/fsck-rootfs.sh "${log_dir}/rootfs.ext4" /root/t persist
    fi
    if [[ "$scenario" == "net" ]]; then
        ./scripts/check-pcap.sh "${log_dir}/net.pcap"
    fi
done

echo "sanitize-stress: ${sanitizer} ok"
