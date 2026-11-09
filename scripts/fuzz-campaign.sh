#!/bin/bash
# Build, codesign (hv-only), and run every cargo-fuzz target for FUZZ_TIME seconds.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
export RUSTUP_HOME="${RUSTUP_HOME:-$ROOT/.fuzz-toolchain/rustup}"
export CARGO_HOME="${CARGO_HOME:-$ROOT/.fuzz-toolchain/cargo}"
export PATH="$CARGO_HOME/bin:$PATH"
unset CARGO_TARGET_DIR
export CARGO_TARGET_DIR="$ROOT/ternvale-fuzz/fuzz/target"

FUZZ_TIME="${FUZZ_TIME:-600}"
TIMEOUT="${TIMEOUT:-5}"
RSS_MB="${RSS_MB:-4096}"
ENT="${TERNVALE_ENTITLEMENTS:-$ROOT/entitlements/ternvale-hv-only.entitlements}"
OUT="$ROOT/ternvale-fuzz/fuzz/campaign"
TARGETS="${FUZZ_TARGETS:-virtqueue virtio_blk virtio_net virtio_rng virtio_vsock virtio_gpu virtio_input virtio_balloon virtio_fs virtio_pci xhci_trb tpm_crb vsock_packet agent_proto esr psci user_net fuse}"

if ! cargo +nightly fuzz --help >/dev/null 2>&1; then
	echo "fuzz-campaign: need rustup nightly and cargo-fuzz in $CARGO_HOME" >&2
	exit 1
fi

mkdir -p "$OUT/logs" "$OUT/artifacts" "$OUT/corpus"
cd "$ROOT/ternvale-fuzz"
echo "fuzz-campaign: building targets"
cargo +nightly fuzz build

ID="${TERNVALE_CODESIGN_IDENTITY:-}"
if [[ -z "$ID" ]]; then
	ID=$(security find-identity -v -p codesigning 2>/dev/null \
		| sed -n 's/.*"\(Apple Development:.*\)"/\1/p' | head -n 1 || true)
fi
if [[ -z "$ID" ]]; then
	ID="-"
fi

bins=()
for t in $TARGETS; do
	bin="$CARGO_TARGET_DIR/aarch64-apple-darwin/release/$t"
	if [[ ! -x "$bin" ]]; then
		echo "fuzz-campaign: missing $bin" >&2
		exit 1
	fi
	codesign --sign "$ID" --force --entitlements "$ENT" "$bin"
	rm -rf "$OUT/corpus/$t" "$OUT/artifacts/$t"
	mkdir -p "$OUT/corpus/$t" "$OUT/artifacts/$t"
	cp -R "$ROOT/ternvale-fuzz/fuzz/corpus/$t/." "$OUT/corpus/$t/"
	bins+=("$bin")
done

echo "fuzz-campaign: starting ${#bins[@]} targets for ${FUZZ_TIME}s"
pids=()
i=0
for t in $TARGETS; do
	bin="$CARGO_TARGET_DIR/aarch64-apple-darwin/release/$t"
	(
		set +e
		"$bin" \
			-max_total_time="$FUZZ_TIME" \
			-timeout="$TIMEOUT" \
			-rss_limit_mb="$RSS_MB" \
			-artifact_prefix="$OUT/artifacts/$t/" \
			"$OUT/corpus/$t" \
			>"$OUT/logs/$t.log" 2>&1
		echo $? >"$OUT/logs/$t.exit"
	) &
	pids+=("$!")
	i=$((i + 1))
done

fail=0
for pid in "${pids[@]}"; do
	if ! wait "$pid"; then
		fail=1
	fi
done

echo "fuzz-campaign: done"
echo "=== summary ==="
for t in $TARGETS; do
	exitc=$(cat "$OUT/logs/$t.exit" 2>/dev/null || echo missing)
	arts=$(find "$OUT/artifacts/$t" -type f 2>/dev/null | wc -l | tr -d ' ')
	done_line=$(grep -E '^(Done |==ERROR|SUMMARY:|out-of-memory|ALARM:)' "$OUT/logs/$t.log" | tail -n 3 | tr '\n' ' ')
	printf '%-16s exit=%s artifacts=%s %s\n' "$t" "$exitc" "$arts" "$done_line"
done
exit "$fail"
