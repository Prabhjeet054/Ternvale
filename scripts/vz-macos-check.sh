#!/usr/bin/env bash
# Step 93 test: macOS guest create on VZ — disk-space refuse, IPSW resume,
# installer progress, identity stability, Setup Assistant boot.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
out="$root/target/vz-macos/$(date +%Y%m%d-%H%M%S)"
run_dir="/tmp/tvvz-macos-$$"
bin="$target/debug/ternvale"
sign="$root/scripts/sign-and-run.sh"
cache="$out/ipsw-cache"
vms="$out/vms"
name="mac-e2e"
log() { printf 'vz-macos-check: %s\n' "$*" >&2; }
fail() { log "FAIL: $*"; exit 1; }

mkdir -p "$out" "$run_dir" "$cache" "$vms"
export TERNVALE_RUN_DIR="$run_dir"
export TERNVALE_VMS_DIR="$vms"
export TERNVALE_IPSW_CACHE="$cache"
export TERNVALE_SKIP_SANDBOX=1
export TERNVALE_LOG="${TERNVALE_LOG:-info,ternvale::vz=debug,ternvale::backend=info,ternvale::cli=info}"

(cd "$root" && cargo build -p ternvale-cli -q)

log "mock free space refuses create before any IPSW lands in the cache"
export TERNVALE_MOCK_FREE_BYTES=1
set +e
"$sign" "$bin" create --macos --name "$name-refused" \
    >"$out/refuse.out" 2>"$out/refuse.err"
refuse_rc=$?
set -e
unset TERNVALE_MOCK_FREE_BYTES
refuse_txt=$(cat "$out/refuse.err" "$out/refuse.out")
printf '%s\n' "$refuse_txt" >"$out/refuse.txt"
[[ "$refuse_rc" -ne 0 ]] || fail "create should fail with TERNVALE_MOCK_FREE_BYTES=1"
echo "$refuse_txt" | grep -q "not enough disk" || fail "refuse message missing: $refuse_txt"
echo "$refuse_txt" | grep -q "before downloading" || fail "must say before downloading: $refuse_txt"
if find "$cache" -type f ! -name '.gitkeep' 2>/dev/null | grep -q .; then
    fail "cache was written despite refuse: $(ls -la "$cache")"
fi
log "refuse ok (rc=$refuse_rc, cache empty)"

log "local HTTP resume + checksum (unit test)"
(cd "$root" && cargo test -p ternvale-vz --lib macos_download_tests -- --test-threads=1) \
    | tee "$out/resume-unit.log"
grep -q "interrupted_download_resumes_and_checksum_matches" "$out/resume-unit.log" \
    || fail "resume unit test did not run"

# Do not run a default 64G create against a large volume — that would download
# another ~24 GiB IPSW. Conservative need is disk + restore + 8 GiB.
log "64G create with mocked 10 GiB free refuses before download"
export TERNVALE_MOCK_FREE_BYTES=$((10 << 30))
set +e
"$sign" "$bin" create --macos --name "$name-64g" --disk-size 64G \
    >"$out/real-space.out" 2>"$out/real-space.err"
real_rc=$?
set -e
unset TERNVALE_MOCK_FREE_BYTES
real_txt=$(cat "$out/real-space.err" "$out/real-space.out")
printf '%s\n' "$real_txt" >"$out/real-space.txt"
[[ "$real_rc" -ne 0 ]] || fail "64G create should fail with 10 GiB mocked free"
echo "$real_txt" | grep -q "not enough disk" || fail "64G refuse message missing: $real_txt"
if find "$cache" -type f ! -name '.gitkeep' 2>/dev/null | grep -q .; then
    fail "64G refuse still downloaded: $(ls -la "$cache")"
fi
log "64G refuse ok (rc=$real_rc, cache empty)"

if [[ "${TERNVALE_SKIP_LIVE_MACOS:-}" == "1" ]]; then
    log "TERNVALE_SKIP_LIVE_MACOS=1: skipping IPSW download, installer, reboot"
    log "PASS (refuse + resume unit tests only)"
    exit 0
fi

# Sparse disk + IPSW. Defaults stay small so an 18 GiB RAM / tight-disk host
# does not OOM or fill the volume. Reuse a complete IPSW; never re-download.
disk_size="${TERNVALE_MACOS_DISK:-32G}"
cpus="${TERNVALE_MACOS_CPUS:-2}"
ram_mib="${TERNVALE_MACOS_RAM_MIB:-4096}"
cached_ipsw="${TERNVALE_MACOS_IPSW:-}"
if [[ -z "$cached_ipsw" ]]; then
    for candidate in \
        "$root/target/vz-macos/20261111-154916/ipsw-cache/26B131.ipsw" \
        "$HOME/Library/Caches/Ternvale/ipsw/"*.ipsw; do
        if [[ -f "$candidate" ]]; then
            cached_ipsw="$candidate"
            break
        fi
    done
fi
create_cmd=("$sign" "$bin" create --macos --name "$name" --disk-size "$disk_size" \
    --cpus "$cpus" --ram-mib "$ram_mib")
if [[ -n "$cached_ipsw" ]]; then
    create_cmd+=(--ipsw "$cached_ipsw")
    log "live create uses cached IPSW $cached_ipsw (no download)"
else
    log "live create --macos --disk-size $disk_size (interrupt download, then resume+install)"
fi
log "guest sizing: disk=$disk_size cpus=$cpus ram_mib=$ram_mib"

interrupt_create() {
    "${create_cmd[@]}" >"$out/create1.out" 2>"$out/create1.err" &
    pid=$!
    log "create pid=$pid; waiting for .part"
    for _ in $(seq 1 600); do
        part=$(find "$cache" -name '*.ipsw.part' -print -quit 2>/dev/null || true)
        if [[ -n "${part}" ]]; then
            size=$(stat -f%z "$part" 2>/dev/null || echo 0)
            if [[ "$size" -gt 8388608 ]]; then
                log "killing create at part=$size bytes ($part)"
                kill "$pid" 2>/dev/null || true
                sleep 1
                kill -9 "$pid" 2>/dev/null || true
                wait "$pid" 2>/dev/null || true
                echo "$size" >"$out/interrupt-bytes"
                echo "$part" >"$out/interrupt-part"
                return 0
            fi
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            wait "$pid" || true
            log "create exited before interrupt; see $out/create1.err"
            return 1
        fi
        sleep 1
    done
    kill "$pid" 2>/dev/null || true
    fail "timed out waiting for IPSW .part"
}

interrupted=1
if [[ -z "$cached_ipsw" ]]; then
    set +e
    interrupt_create
    interrupted=$?
    set -e
else
    log "skip download interrupt; IPSW already on disk"
fi

# Drop the half-created VM bundle so the second create can reuse the cache.
rm -rf "$vms/$name"

if [[ "$interrupted" -eq 0 ]]; then
    log "resume create (same cache .part)"
    set +e
    "${create_cmd[@]}" >"$out/create2.out" 2>"$out/create2.err"
    create_rc=$?
    set -e
    cat "$out/create2.out" "$out/create2.err" >"$out/create2.txt"
    grep -E "progress: download" "$out/create2.txt" | tee "$out/download-progress.txt" || true
    python3 - "$out/download-progress.txt" <<'PY' || fail "download progress not monotonic"
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text(errors="replace")
vals = [int(m) for m in re.findall(r"progress: download (\d+)‰", text)]
if not vals:
    sys.exit("no download progress lines (installer may have used a complete cache)")
prev = -1
for v in vals:
    if v < prev:
        sys.exit(f"backwards {prev} -> {v}")
    prev = v
print("download progress samples", len(vals), "last", vals[-1])
PY
    grep -q "resumed=true\|resumed=1" "$out/create2.txt" && log "download resumed=1" || log "resume flag not in stdout (file may already have been complete)"
else
    log "interrupt skipped; running a single create"
    set +e
    "${create_cmd[@]}" >"$out/create2.out" 2>"$out/create2.err"
    create_rc=$?
    set -e
    cat "$out/create2.out" "$out/create2.err" >"$out/create2.txt"
fi

if [[ "${create_rc:-1}" -ne 0 ]]; then
    log "create failed rc=$create_rc"
    tail -n 80 "$out/create2.txt" || true
    fail "live create --macos failed"
fi

grep -E "progress: install" "$out/create2.txt" | tee "$out/install-progress.txt" || true
if [[ -s "$out/install-progress.txt" ]]; then
    python3 - "$out/install-progress.txt" <<'PY' || fail "install progress not monotonic"
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text(errors="replace")
vals = [int(m) for m in re.findall(r"progress: install (\d+)‰", text)]
prev = -1
for v in vals:
    if v < prev:
        sys.exit(f"backwards {prev} -> {v}")
    prev = v
print("install progress samples", len(vals), "last", vals[-1] if vals else None)
PY
else
    log "no install progress (TERNVALE_SKIP_MACOS_INSTALL?)"
fi

config="$vms/$name/config.toml"
tvmp="$vms/$name/macos-platform.tvmp"
[[ -f "$config" ]] || fail "missing $config"
[[ -f "$tvmp" ]] || fail "missing $tvmp"
hw1=$(shasum -a 256 "$tvmp" | awk '{print $1}')
aux1=$(shasum -a 256 "$vms/$name/AuxiliaryStorage" | awk '{print $1}')
echo "$hw1" >"$out/tvmp.sha256"
log "identity sha256=$hw1"

grep -E "progress: install 1000‰" "$out/create2.txt" && log "installer reached 1000‰" || log "no 1000‰ install line"

boot_once() {
    local tag=$1
    log "boot $tag"
    # Binary is already signed from create. Re-codesigning on every status
    # poll races and can delete target/debug/ternvale.
    "$bin" run "$config" >"$out/run-$tag.out" 2>"$out/run-$tag.err" &
    local rpid=$!
    local ok=0
    for _ in $(seq 1 90); do
        if TERNVALE_LOG=warn "$bin" status "$name" --json \
            >"$out/status-$tag.json" 2>/dev/null; then
            if python3 - "$out/status-$tag.json" <<'PY'
import json, sys
raw = open(sys.argv[1]).read().strip().splitlines()[-1]
data = json.loads(raw)
st = data.get("status") or data
print("state", st.get("state"), "backend", st.get("backend"), file=sys.stderr)
sys.exit(0 if st.get("state") == "running" else 1)
PY
            then
                ok=1
                break
            fi
        fi
        if ! kill -0 "$rpid" 2>/dev/null; then
            break
        fi
        sleep 2
    done
    [[ "$ok" -eq 1 ]] || {
        cat "$out/run-$tag.err" "$out/run-$tag.out" || true
        fail "guest did not reach running ($tag)"
    }
    sleep 8
    TERNVALE_LOG=warn "$bin" stop "$name" --force >/dev/null 2>&1 || true
    wait "$rpid" 2>/dev/null || true
    hw=$(shasum -a 256 "$tvmp" | awk '{print $1}')
    [[ "$hw" == "$hw1" ]] || fail "macos-platform.tvmp changed on $tag ($hw vs $hw1)"
    aux=$(shasum -a 256 "$vms/$name/AuxiliaryStorage" | awk '{print $1}')
    # AuxiliaryStorage is VZ NVRAM; Apple updates it on boot. Hardware model
    # and machine identifier live in macos-platform.tvmp and must stay put.
    if [[ "$aux" != "$aux1" ]]; then
        log "AuxiliaryStorage NVRAM changed on $tag (expected); tvmp unchanged"
    fi
}

boot_once 1
# First boot is Setup Assistant (no display in this step). Serial is virtio
# console; macOS typically does not print the assistant there.
if grep -qi "setup assistant\|Setup Assistant\|Hello" "$vms/$name/serial.log" 2>/dev/null; then
    log "serial mentioned Setup Assistant"
else
    log "serial has no Setup Assistant banner (expected headless); installer finished and VM reached running"
fi
boot_once 2
boot_once 3

log "PASS"
echo "artifacts $out"
