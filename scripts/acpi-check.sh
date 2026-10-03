#!/bin/bash
# Check every ACPI table Ternvale generates, acpidump-style, and the DSDT with iasl.
#
#   ./scripts/acpi-check.sh                 # build the tables and check them
#   ./scripts/acpi-check.sh --set DIR ...   # check table sets already on disk
#                                           # (e.g. from `ternvale acpi-dump`)
#
# Without --set, `ternvale acpi-dump --offline` writes one table set per vCPU
# count in ACPI_CHECK_CPUS (default "1 4 123"; 123 is the most the GIC
# redistributor range holds) under target/acpi-check/cpus-N/ternvale/. Offline
# means the ACPI window is a host buffer walked from the RSDP exactly as the
# VMM walks guest memory: same bytes and addresses, no VM, no entitlement.
#
# Every *.dat file in a set must have:
#   - SDTs: its signature in bytes 0..4 (the file name), a length field equal
#     to the file size (>= 36), and a byte sum of 0 mod 256 (ACPI 6.5 §5.2.6);
#   - RSDP: "RSD PTR ", revision 2, length 36, both checksums 0 (§5.2.5.3).
# The pointers must hold together: the RSDP's XsdtAddress is the XSDT's
# address, every XSDT entry is the address of a dumped table and every table
# but RSDP/XSDT/DSDT is listed, and the FADT's X_DSDT is the DSDT's address.
# Addresses come from the set's tables.txt.
#
# Then, if iasl is installed: `iasl -d DSDT.dat` must succeed with no
# error/warning lines, recompiling the disassembly must report 0 Errors and
# 0 Warnings, and an `iasl -oa` recompile must equal DSDT.dat after the 36-byte
# header (the header differs only in compiler ID and checksum). Warnings count
# because they are real ASL problems (QEMU's own DSDT trips one, "Not all
# control paths return a value" in EDSM, so --set on QEMU's tables fails);
# remarks are only reported. The round trip catches AML iasl parses without
# complaint but not as built, such as a package running past the table end.
#
# Without iasl the DSDT step is skipped with a SKIP line and the script still
# exits 0 if the other checks pass. TERNVALE_REQUIRE_IASL=1 makes a missing
# iasl a failure. IASL=<path> picks the binary. Exits 1 on any failure.
#
# A checksum failure names the file, signature, table address and the
# checksum byte (offset, address, current and correct value).
# ACPI_CHECK_CORRUPT=<SIG> (e.g. GTDT, or RSDP) proves that: it builds the CLI
# with the acpi-fault-injection feature, which adds 1 to that table's checksum
# byte before the set is written, and the run must then fail (it also fails,
# differently, if nothing catches the corruption). The next plain run rebuilds
# the CLI without the feature.
#
# Multi-byte fields are read with od in host byte order: little-endian on
# Apple silicon and on the x86-64/arm64 CI hosts.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

log() { printf 'acpi-check: %s\n' "$*" >&2; }
failures=0
problem() {
    log "FAIL: $*"
    failures=$((failures + 1))
}

# Tables every Ternvale set has (ternvale_acpi::SIGNATURES, file names).
generated=(RSDP XSDT FACP DSDT APIC GTDT MCFG SPCR DBG2)

sets=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --set)
            [[ $# -ge 2 ]] || { log "--set needs a directory"; exit 2; }
            sets+=("$2")
            shift 2
            ;;
        -h | --help)
            awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
            exit 0
            ;;
        *)
            log "unknown argument $1 (see --help)"
            exit 2
            ;;
    esac
done

iasl_bin="${IASL:-$(command -v iasl || true)}"
if [[ -n "$iasl_bin" ]]; then
    log "iasl: ${iasl_bin} ($("$iasl_bin" -v 2>&1 | grep -m1 -o 'version [0-9]*' || echo 'version unknown'))"
elif [[ "${TERNVALE_REQUIRE_IASL:-0}" == 1 ]]; then
    log "FAIL: iasl not found and TERNVALE_REQUIRE_IASL=1"
    exit 1
else
    log "SKIP: iasl not installed; DSDT disassembly not checked (brew install --formula homebrew/core/acpica). Header, checksum and pointer checks still run."
fi

# Byte sum mod 256 of the first $2 bytes of $1 (all bytes when $2 is empty).
byte_sum() {
    local file="$1" count="${2:-}"
    if [[ -n "$count" ]]; then
        head -c "$count" "$file" | od -An -v -tu1
    else
        od -An -v -tu1 "$file"
    fi | awk '{ for (i = 1; i <= NF; i++) s += $i } END { print s % 256 }'
}
u32_at() { od -An -tu4 -j "$2" -N 4 "$1" | tr -d ' \n'; }
u64_at() { od -An -tu8 -j "$2" -N 8 "$1" | tr -d ' \n'; }

# The current set's tables.txt as parallel arrays (macOS /bin/bash 3.2 has no
# associative arrays).
names=()
addrs=()
gpa_of() {
    local i
    for ((i = 0; i < ${#names[@]}; i++)); do
        if [[ "${names[$i]}" == "$1" ]]; then
            echo "${addrs[$i]}"
            return
        fi
    done
}
name_at() {
    local i
    for ((i = 0; i < ${#addrs[@]}; i++)); do
        if [[ "${addrs[$i]}" == "$1" ]]; then
            echo "${names[$i]}"
            return
        fi
    done
}

# Header and checksum checks for one SDT file; $2 is the expected signature.
check_sdt() {
    local file="$1" want="$2" size sig len sum
    size=$(wc -c <"$file" | tr -d ' ')
    if [[ "$size" -lt 36 ]]; then
        problem "${file}: ${size} bytes, shorter than the 36-byte SDT header"
        return
    fi
    sig=$(head -c 4 "$file")
    len=$(u32_at "$file" 4)
    sum=$(byte_sum "$file")
    [[ "$sig" == "$want" ]] || problem "${file}: signature '${sig}', file name says '${want}'"
    [[ "$len" == "$size" ]] || problem "${file}: length field ${len}, file is ${size} bytes"
    # The SDT checksum byte is offset 9 (ACPI 6.5 Table 5.4).
    [[ "$sum" == 0 ]] || checksum_problem "$file" "$sig" 9 "$sum" "checksum"
}

# A checksum failure naming the table, its address and the checksum byte:
# $1 file, $2 signature, $3 checksum byte offset, $4 byte sum mod 256, $5 label.
checksum_problem() {
    local file="$1" sig="$2" offset="$3" sum="$4" label="$5" gpa table="" byte="" stored
    gpa=$(gpa_of "$(basename "$file" .dat)")
    stored=$(od -An -tu1 -j "$offset" -N 1 "$file" | tr -d ' \n')
    if [[ -n "$gpa" ]]; then
        table=" (table at $(printf '%#x' "$gpa"))"
        byte=" (gpa $(printf '%#x' $((gpa + offset))))"
    fi
    problem "$(printf "%s: %s%s %s wrong: bytes sum to %d mod 256, want 0; checksum byte at offset %d%s is 0x%02x, 0x%02x would be correct" \
        "$file" "$sig" "$table" "$label" "$sum" "$offset" "$byte" "$stored" $(((stored - sum + 256) % 256)))"
}

check_rsdp() {
    local file="$1" size sum20 sum36
    size=$(wc -c <"$file" | tr -d ' ')
    if [[ "$size" -ne 36 ]]; then
        problem "${file}: ${size} bytes, an ACPI 2.0+ RSDP is 36"
        return
    fi
    [[ "$(head -c 8 "$file")" == "RSD PTR " ]] || problem "${file}: signature is not 'RSD PTR '"
    [[ "$(od -An -tu1 -j 15 -N 1 "$file" | tr -d ' \n')" == 2 ]] || problem "${file}: revision is not 2"
    [[ "$(u32_at "$file" 20)" == 36 ]] || problem "${file}: Length field is not 36"
    sum20=$(byte_sum "$file" 20)
    sum36=$(byte_sum "$file")
    # Checksum at offset 8 covers bytes 0..20; extended checksum at 32 covers all 36 (Table 5.3).
    [[ "$sum20" == 0 ]] || checksum_problem "$file" "RSDP" 8 "$sum20" "checksum (first 20 bytes)"
    [[ "$sum36" == 0 ]] || checksum_problem "$file" "RSDP" 32 "$sum36" "extended checksum"
}

check_set() {
    local set="$1" strict="$2" listing="$1/tables.txt"
    if [[ ! -f "$listing" ]]; then
        problem "${set}: no tables.txt (not an acpi-dump set)"
        return
    fi
    # tables.txt: "  SIG  0xGPA  LEN B  rev N  checksum ok"; the RSDP is "RSD PTR".
    names=()
    addrs=()
    local name address
    while read -r name address; do
        names+=("$name")
        addrs+=("$((address))")
    done < <(awk '$1 == "RSD" { print "RSDP", $3; next } NF { print $1, $2 }' "$listing")
    local sig gpa
    while read -r sig gpa; do
        problem "${listing}: acpi-dump marked ${sig} (table at $(printf '%#x' "$((gpa))")) checksum BAD"
    done < <(awk '/checksum BAD/ { if ($1 == "RSD") print "RSDP", $3; else print $1, $2 }' "$listing")
    local required=(RSDP XSDT FACP DSDT)
    [[ "$strict" == 1 ]] && required=("${generated[@]}")
    for name in "${required[@]}"; do
        [[ -f "${set}/${name}.dat" && -n "$(gpa_of "$name")" ]] || problem "${set}: ${name} missing"
    done
    for name in ${names[@]+"${names[@]}"}; do
        [[ -f "${set}/${name}.dat" ]] || problem "${set}: ${name} is in tables.txt but ${name}.dat is missing"
    done
    local xsdt_gpa dsdt_gpa
    xsdt_gpa=$(gpa_of XSDT)
    dsdt_gpa=$(gpa_of DSDT)

    local file count=0
    for file in "$set"/*.dat; do
        [[ -e "$file" ]] || continue
        name=$(basename "$file" .dat)
        count=$((count + 1))
        if [[ "$name" == RSDP ]]; then
            check_rsdp "$file"
        else
            check_sdt "$file" "${name%%-*}"
        fi
    done

    if [[ -n "$xsdt_gpa" && -f "${set}/RSDP.dat" ]]; then
        [[ "$(u64_at "${set}/RSDP.dat" 24)" == "$xsdt_gpa" ]] ||
            problem "${set}: RSDP XsdtAddress is not the XSDT's address $(printf '%#x' "$xsdt_gpa")"
    fi
    if [[ -f "${set}/XSDT.dat" ]]; then
        local xsdt="${set}/XSDT.dat" len entries i entry found listed=" "
        len=$(wc -c <"$xsdt" | tr -d ' ')
        if (((len - 36) % 8 != 0)); then
            problem "${xsdt}: $((len - 36)) entry bytes is not a whole number of 8-byte pointers"
        fi
        entries=$(((len - 36) / 8))
        for ((i = 0; i < entries; i++)); do
            entry=$(u64_at "$xsdt" $((36 + 8 * i)))
            found=$(name_at "$entry")
            if [[ -z "$found" ]]; then
                problem "${xsdt}: entry ${i} points at $(printf '%#x' "$entry"), which is no dumped table"
            else
                listed+="${found} "
            fi
        done
        for name in ${names[@]+"${names[@]}"}; do
            case "$name" in RSDP | XSDT | DSDT) continue ;; esac
            [[ "$listed" == *" ${name} "* ]] || problem "${xsdt}: ${name} is not listed"
        done
    fi
    if [[ -f "${set}/FACP.dat" && -n "$dsdt_gpa" ]]; then
        # FADT X_DSDT at offset 140 (ACPI 6.5 Table 5.9).
        [[ "$(u64_at "${set}/FACP.dat" 140)" == "$dsdt_gpa" ]] ||
            problem "${set}: FADT X_DSDT is not the DSDT's address $(printf '%#x' "$dsdt_gpa")"
    fi
    log "${set}: ${count} tables, headers/checksums/pointers checked"
}

# iasl round trip of <set>/DSDT.dat.
check_dsdt_iasl() {
    local set="$1" text status summary disasm="iasl -d clean"
    [[ -f "${set}/DSDT.dat" ]] || return 0
    rm -f "${set}/DSDT.dsl" "${set}/DSDT-recompiled.aml" "${set}/DSDT-oa.aml"
    set +e
    text=$(cd "$set" && "$iasl_bin" -d DSDT.dat 2>&1)
    status=$?
    set -e
    if [[ "$status" -ne 0 || "$text" != *"Disassembly completed"* ]]; then
        problem "${set}: iasl -d DSDT.dat failed (exit ${status}):"$'\n'"${text}"
        return
    fi
    if grep -iE 'error|warning|incorrect' <<<"$text" | grep -v '^Compilation successful' >&2; then
        problem "${set}: iasl -d DSDT.dat reported the lines above"
        disasm="iasl -d FAILED (see above)"
    fi
    set +e
    text=$(cd "$set" && "$iasl_bin" -p DSDT-recompiled DSDT.dsl 2>&1)
    status=$?
    set -e
    summary=$(grep -m1 -E '[0-9]+ Errors, [0-9]+ Warnings' <<<"$text" || true)
    if [[ "$status" -ne 0 || ! "$summary" =~ \ 0\ Errors,\ 0\ Warnings ]]; then
        problem "${set}: recompiling DSDT.dsl: exit ${status}, '${summary:-no summary}':"$'\n'"${text}"
        return
    fi
    set +e
    text=$(cd "$set" && "$iasl_bin" -oa -p DSDT-oa DSDT.dsl 2>&1)
    status=$?
    set -e
    if [[ "$status" -ne 0 ]]; then
        problem "${set}: iasl -oa DSDT.dsl failed (exit ${status}):"$'\n'"${text}"
        return
    fi
    if ! cmp -s <(tail -c +37 "${set}/DSDT.dat") <(tail -c +37 "${set}/DSDT-oa.aml"); then
        problem "${set}: iasl -oa recompile differs from DSDT.dat after the header"
        return
    fi
    log "${set}: DSDT $(wc -c <"${set}/DSDT.dat" | tr -d ' ') bytes: ${disasm}, ${summary#*. }, -oa round trip identical"
}

strict=0
corrupt="${ACPI_CHECK_CORRUPT:-}"
if [[ -n "$corrupt" && ${#sets[@]} -ne 0 ]]; then
    log "ACPI_CHECK_CORRUPT applies to generated tables, not --set"
    exit 2
fi
if [[ ${#sets[@]} -eq 0 ]]; then
    strict=1
    out="${root}/target/acpi-check"
    rm -rf "$out"
    mkdir -p "$out"
    build=(cargo build -q -p ternvale-cli)
    if [[ -n "$corrupt" ]]; then
        log "FAULT INJECTION: test build corrupts the ${corrupt} checksum; this run must fail"
        build+=(--features acpi-fault-injection)
    fi
    log "building ternvale-cli"
    "${build[@]}"
    cli="${CARGO_TARGET_DIR:-${root}/target}/debug/ternvale"
    for cpus in ${ACPI_CHECK_CPUS:-1 4 123}; do
        if ! TERNVALE_ACPI_CORRUPT="$corrupt" TERNVALE_LOG="${TERNVALE_LOG:-warn}" "$cli" acpi-dump --offline \
            --out "${out}/cpus-${cpus}" --cpus "$cpus" >"${out}/cpus-${cpus}.txt" 2>&1; then
            problem "acpi-dump --offline --cpus ${cpus} failed:"$'\n'"$(cat "${out}/cpus-${cpus}.txt")"
            continue
        fi
        sets+=("${out}/cpus-${cpus}/ternvale")
    done
fi

for set in ${sets[@]+"${sets[@]}"}; do
    check_set "$set" "$strict"
    if [[ -n "$iasl_bin" ]]; then
        check_dsdt_iasl "$set"
    fi
done

if [[ -n "$corrupt" && "$failures" -eq 0 ]]; then
    problem "ACPI_CHECK_CORRUPT=${corrupt} corrupted a checksum and nothing caught it"
fi
if [[ "$failures" -ne 0 ]]; then
    log "FAILED: ${failures} problem(s) in ${#sets[@]} table set(s)"
    exit 1
fi
log "OK: ${#sets[@]} table set(s)$([[ -z "$iasl_bin" ]] && echo ', DSDT iasl check skipped (no iasl)')"
