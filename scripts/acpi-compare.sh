#!/bin/bash
# Compare Ternvale's ACPI tables with QEMU `-M virt`'s, by signature.
#
#   ./scripts/acpi-compare.sh
#
# 1. QEMU (-M virt,gic-version=3 like Ternvale, -cpu max, 256 MiB, TCG, its
#    bundled EDK2) boots to the UEFI shell. By then EDK2 has installed QEMU's
#    tables in RAM; the monitor saves all of RAM with pmemsave.
# 2. `ternvale acpi-dump` builds a probe VM's ACPI window exactly as a boot does,
#    reads the tables back from guest memory into ternvale/, walks the QEMU image
#    into qemu/, and writes compare.md.
# 3. signatures.diff is `diff -u` of the two signature lists. With iasl on PATH,
#    every dumped table except the RSDP (iasl cannot read a standalone RSDP) is
#    disassembled; iasl.txt keeps its problem lines, including a table that ends
#    mid-structure. iasl 20260408 reads every SPCR with its revision 4 template,
#    so the revision 2 SPCR both VMMs emit (80 bytes) is expected to "terminate"
#    at offset 0x50; that one case is not a problem.
# Output: target/acpi-compare/<stamp>/ (the RAM image is deleted unless
# KEEP_RAM=1). Exits 1 if a step fails or iasl reports a
# problem in Ternvale's tables. Signature differences are expected (QEMU also
# has PPTT and IORT) and do not fail the run.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

QEMU=${QEMU:-qemu-system-aarch64}
if ! command -v "$QEMU" >/dev/null; then
    printf 'acpi-compare: %s not found (brew install qemu)\n' "$QEMU" >&2
    exit 2
fi
FIRMWARE=${QEMU_EFI:-"$(dirname "$(command -v "$QEMU")")/../share/qemu/edk2-aarch64-code.fd"}
if [[ ! -f "$FIRMWARE" ]]; then
    printf 'acpi-compare: no EDK2 image at %s (set QEMU_EFI)\n' "$FIRMWARE" >&2
    exit 2
fi
RAM_BASE=0x40000000
RAM_SIZE=0x10000000
LIMIT=90

out_dir="${root}/target/acpi-compare/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out_dir"
cargo build -q -p ternvale-cli
ternvale="${CARGO_TARGET_DIR:-${root}/target}/debug/ternvale"
codesign --sign - --force --entitlements entitlements/ternvale.entitlements "$ternvale" 2>/dev/null

# 1. QEMU to the UEFI shell, then save RAM.
serial="${out_dir}/qemu-serial.log"
monitor="${out_dir}/qemu-monitor.sock"
ram="${out_dir}/qemu-ram.bin"
"$QEMU" -M virt,gic-version=3 -cpu max -m 256M -accel tcg -bios "$FIRMWARE" -display none -nic none \
    -serial "file:${serial}" -monitor "unix:${monitor},server,nowait" -no-reboot &
qemu_pid=$!
trap 'kill "$qemu_pid" 2>/dev/null || true' EXIT
start=$(date +%s)
until grep -q 'Shell>' "$serial" 2>/dev/null; do
    if (( $(date +%s) - start > LIMIT )); then
        printf 'acpi-compare: QEMU did not reach the UEFI shell in %ss (see %s)\n' "$LIMIT" "$serial" >&2
        exit 1
    fi
    sleep 1
done
printf 'acpi-compare: QEMU at the UEFI shell after %ss; saving RAM\n' "$(( $(date +%s) - start ))"
# HMP parses the size as an expression, so the path must be quoted.
{ printf 'pmemsave %s %s "%s"\n' "$RAM_BASE" "$RAM_SIZE" "$ram"; sleep 4; printf 'quit\n'; sleep 1; } |
    nc -U "$monitor" >"${out_dir}/qemu-monitor.txt" 2>&1 || true
wait "$qemu_pid" 2>/dev/null || true
trap - EXIT
if [[ ! -s "$ram" ]]; then
    printf 'acpi-compare: pmemsave wrote nothing (see %s)\n' "${out_dir}/qemu-monitor.txt" >&2
    exit 1
fi

# 2. Ternvale's tables from guest memory, QEMU's from the image, compare.md.
"$ternvale" acpi-dump --out "$out_dir" --qemu-ram "$ram" --ram-base "$RAM_BASE" |
    tee "${out_dir}/acpi-dump.txt"
# The 256 MiB image is only needed to re-walk; KEEP_RAM=1 keeps it.
[[ "${KEEP_RAM:-0}" == 1 ]] || rm -f "$ram"

# 3. Signature diff and iasl.
signatures() { (cd "$1" && ls ./*.dat | sed 's|^\./||; s|\.dat$||' | sort); }
signatures "${out_dir}/ternvale" >"${out_dir}/ternvale-signatures.txt"
signatures "${out_dir}/qemu" >"${out_dir}/qemu-signatures.txt"
diff -u --label ternvale --label qemu "${out_dir}/ternvale-signatures.txt" \
    "${out_dir}/qemu-signatures.txt" >"${out_dir}/signatures.diff" || true
printf '\nsignature diff (ternvale vs qemu):\n'
cat "${out_dir}/signatures.diff"

status=0
if command -v iasl >/dev/null; then
    : >"${out_dir}/iasl.txt"
    for side in ternvale qemu; do
        for table in "${out_dir}/${side}"/*.dat; do
            [[ "$(basename "$table")" == RSDP.dat ]] && continue
            log=$(cd "$(dirname "$table")" && iasl -d "$(basename "$table")" 2>&1) || {
                printf '%s: iasl -d failed\n' "${side}/$(basename "$table")" >>"${out_dir}/iasl.txt"
                [[ $side == ternvale ]] && status=1
                continue
            }
            problems=$(grep -iE 'warning|error|incorrect' <<<"$log" || true)
            ends=$(grep -A1 'terminates in the middle' "${table%.dat}.dsl" 2>/dev/null | tr '\n' ' ' || true)
            if [[ -n "$ends" ]] && ! [[ "$(basename "$table")" == SPCR.dat &&
                "$ends" == *'CurrentOffset: 50, TableLength: 50'* ]]; then
                problems="${problems:+${problems}$'\n'}${ends}"
            fi
            if [[ -n "$problems" ]]; then
                printf '%s:\n%s\n' "${side}/$(basename "$table")" "$problems" >>"${out_dir}/iasl.txt"
                [[ $side == ternvale ]] && status=1
            fi
        done
    done
    printf '\niasl -d: %s\n' "$( [[ -s "${out_dir}/iasl.txt" ]] && cat "${out_dir}/iasl.txt" || echo 'no problems in either set')"
else
    printf '\niasl not on PATH; skipped disassembly (brew install acpica)\n'
fi
printf '\nacpi-compare: results in %s\n' "$out_dir"
exit "$status"
