#!/bin/bash
# TPM drills through the real CLI: doctor's swtpm check, a VM with the TPM
# disabled, and swtpm killed mid-boot.
#
#   ./scripts/tpm-drill.sh                  # doctor disabled kill
#   ./scripts/tpm-drill.sh doctor           # one or more of: doctor disabled kill
#
# doctor   — `ternvale doctor` with no config and with three VM configs ([tpm]
#            backend auto, backend swtpm, enabled = false). The expected verdicts
#            depend on whether swtpm is installed (PATH, /opt/homebrew/bin,
#            /usr/local/bin); run it once with swtpm and once without
#            (`brew unlink swtpm`, then `brew link swtpm`; without
#            HOMEBREW_NO_INSTALL_FROM_API, which makes brew clone homebrew-core).
# disabled — EDK2 + Linux with `[tpm] enabled = false`. `ps` is sampled every
#            0.2 s while the VM runs: `ternvale run` must never have a swtpm
#            child. The host log's ACPI read-back must list no TPM2, and the
#            guest must find no /sys/firmware/acpi/tables/TPM2 and no /dev/tpmrm0.
# kill     — the same VM with `[tpm] backend = "swtpm"`. swtpm gets SIGKILL when
#            the EFI stub starts the kernel. The host log must have an ERROR
#            naming swtpm's death, and `ternvale report`'s tpm-commands.txt must
#            hold the commands before the failure and the failed ones.
#
# The guest is the `uefi-linux-tpm` boot scenario's ESP (its /init probes the
# TPM, runs tpm2-tools, prints `ternvale-tpm: …` lines and powers off); run
# `TERNVALE_BOOT_SCENARIO=uefi-linux-tpm ./scripts/boot-test.sh` once first, or
# set TPM_DRILL_ESP. Outputs go to target/tpm-drill/<stamp>/<drill>/;
# results.md has one row per check. Exits 1 if any check fails.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

drills="${*:-doctor disabled kill}"
firmware="${root}/test-assets/firmware/QEMU_EFI.fd"
esp="${TPM_DRILL_ESP:-$(ls -t "${root}"/target/boot-logs/*-uefi-linux-tpm/esp.img 2>/dev/null | head -1 || true)}"
log_root="${HOME}/Library/Logs/Ternvale"
out_dir="${root}/target/tpm-drill/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out_dir"
results="${out_dir}/results.md"

cargo build -q -p ternvale-cli
ternvale="${CARGO_TARGET_DIR:-${root}/target}/debug/ternvale"
codesign --sign - --force --entitlements entitlements/ternvale.entitlements "$ternvale" 2>/dev/null
export TERNVALE_LOG="${TERNVALE_LOG:-info}"

# The host log file carries ANSI colour codes; BSD sed needs the ESC byte itself.
strip() { sed $'s/\x1b\\[[0-9;]*m//g' "$@"; }

failed=0
printf '| Drill | Check | Result | Evidence |\n|---|---|---|---|\n' >"$results"

# first_match <file> <regex>: the first matching line, ANSI stripped. Not a
# pipe: `grep -m1` quitting early would SIGPIPE sed, and pipefail fails it.
first_match() {
    local text
    text=$(strip "$1" 2>/dev/null) || return 1
    grep -Em1 -- "$2" <<<"$text"
}

# check <drill> <label> <file> <regex>: the file (ANSI stripped) must match.
check() {
    local evidence
    if evidence=$(first_match "$3" "$4"); then
        printf 'tpm-drill:   ok      %s\n' "$2"
        printf '| %s | %s | ok | `%s` |\n' "$1" "$2" "$(cut -c1-160 <<<"${evidence//|/\\|}")" >>"$results"
    else
        printf 'tpm-drill:   FAILED  %s (/%s/ not in %s)\n' "$2" "$4" "$3"
        printf '| %s | %s | FAILED | /%s/ not in %s |\n' "$1" "$2" "${4//|/\\|}" "${3#"${root}/"}" >>"$results"
        failed=$((failed + 1))
    fi
}

# refute <drill> <label> <file> <regex>: the file must not match.
refute() {
    local evidence
    if evidence=$(first_match "$3" "$4"); then
        printf 'tpm-drill:   FAILED  %s (found: %s)\n' "$2" "$evidence"
        printf '| %s | %s | FAILED | `%s` |\n' "$1" "$2" "$(cut -c1-160 <<<"${evidence//|/\\|}")" >>"$results"
        failed=$((failed + 1))
    else
        printf 'tpm-drill:   ok      %s\n' "$2"
        printf '| %s | %s | ok | no /%s/ in %s |\n' "$1" "$2" "${4//|/\\|}" "$(basename "$3")" >>"$results"
    fi
}

# write_config <dir> <name> <disk> <tpm section lines...>
write_config() {
    local dir=$1 name=$2 disk=$3
    shift 3
    {
        printf 'name = "%s"\ncpus = 2\nram_mib = 1024\n' "$name"
        printf 'serial_log = "%s/guest-serial.log"\n' "$dir"
        printf 'firmware = "%s"\nnvram = "%s/nvram.fd"\nfirmware_tables = "acpi"\n' "$firmware" "$dir"
        printf '\n[[disks]]\npath = "%s"\nread_only = true\n\n[tpm]\n' "$disk"
        printf '%s\n' "$@"
    } >"${dir}/vm.toml"
}

# wait_for <seconds> <file> <text>: 0 once the file contains text.
wait_for() {
    local deadline=$((SECONDS + $1))
    while ((SECONDS < deadline)); do
        if [[ -f "$2" ]] && grep -q -- "$3" "$2"; then
            return 0
        fi
        sleep 0.2
    done
    return 1
}

# sample_ps <pid> <out>: every 0.2 s while pid lives, one line with its children.
sample_ps() {
    local n=0
    while kill -0 "$1" 2>/dev/null; do
        n=$((n + 1))
        local children
        children=$(ps -A -o pid=,ppid=,comm= | awk -v p="$1" '$2 == p { printf "%s:%s ", $1, $3 }')
        printf 'sample=%d swtpm_children=%d swtpm_anywhere=%d children=%s\n' "$n" \
            "$(grep -o 'swtpm' <<<"$children" | wc -l | tr -d ' ')" \
            "$(ps -A -o comm= | grep -c '/swtpm$' || true)" "${children:-none}" >>"$2"
        sleep 0.2
    done
}

# boot <dir> <name>: start the VM with a FIFO as its terminal, type the kernel
# command at the EFI shell. Sets run_pid and sampler_pid; fd 3 is the FIFO.
boot() {
    local dir=$1 name=$2
    mkfifo "${dir}/stdin"
    "$ternvale" run "${dir}/vm.toml" <"${dir}/stdin" >"${dir}/run.stdout" 2>"${dir}/run.stderr" &
    run_pid=$!
    exec 3>"${dir}/stdin"
    sample_ps "$run_pid" "${dir}/ps.txt" &
    sampler_pid=$!
    if ! wait_for 90 "${dir}/guest-serial.log" "Shell>"; then
        printf 'tpm-drill:   no EFI shell prompt within 90 s\n'
        return 1
    fi
    local command='fs0:\IMAGE.EFI initrd=\INITRD console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init printk.devkmsg=on'
    local i
    for ((i = 0; i < ${#command}; i += 8)); do
        printf '%s' "${command:i:8}" >&3
        sleep 0.1
    done
    printf '\r' >&3
}

# finish <dir> <name>: wait for the guest's `done` and the run to exit.
finish() {
    local dir=$1 name=$2
    if ! wait_for 180 "${dir}/guest-serial.log" "ternvale-tpm: done"; then
        printf 'tpm-drill:   guest did not finish within 180 s; stopping it\n'
        "$ternvale" stop "$name" --force >/dev/null 2>&1 || true
    fi
    local deadline=$((SECONDS + 30))
    while kill -0 "$run_pid" 2>/dev/null && ((SECONDS < deadline)); do
        sleep 0.2
    done
    exec 3>&-
    wait "$run_pid" || true
    wait "$sampler_pid" 2>/dev/null || true
    host_log=$(ls -t "${log_root}/ternvale-${name}-"*.log 2>/dev/null | head -1 || true)
    if [[ -n "$host_log" ]]; then
        cp "$host_log" "${dir}/host.log"
    fi
}

prepare_vm() {
    local dir=$1
    if [[ -z "$esp" || ! -f "$esp" ]]; then
        printf 'tpm-drill: no uefi-linux-tpm ESP (run TERNVALE_BOOT_SCENARIO=uefi-linux-tpm ./scripts/boot-test.sh, or set TPM_DRILL_ESP)\n' >&2
        exit 2
    fi
    cp "$esp" "${dir}/esp.img"
}

drill_doctor() {
    local dir="${out_dir}/doctor"
    mkdir -p "$dir"/{auto,swtpm,disabled}
    # Only the swtpm row matters here; any existing file keeps the disk check quiet.
    local disk="${esp:-$firmware}"
    [[ -f "$disk" ]] || disk="$firmware"
    write_config "${dir}/auto" drill-doctor "$disk" 'backend = "auto"'
    write_config "${dir}/swtpm" drill-doctor "$disk" 'backend = "swtpm"'
    write_config "${dir}/disabled" drill-doctor "$disk" 'enabled = false'
    local installed=no
    if command -v swtpm >/dev/null || [[ -x /opt/homebrew/bin/swtpm || -x /usr/local/bin/swtpm ]]; then
        installed=yes
    fi
    local drill="doctor (swtpm installed: ${installed})"
    printf 'tpm-drill: %s\n' "$drill"
    set +e
    "$ternvale" doctor >"${dir}/no-config.txt" 2>"${dir}/no-config.stderr"
    for kind in auto swtpm disabled; do
        "$ternvale" doctor --config "${dir}/${kind}/vm.toml" >"${dir}/${kind}.txt" 2>"${dir}/${kind}.stderr"
    done
    set -e
    for file in no-config auto swtpm disabled; do
        printf -- '--- doctor %s ---\n' "$file"
        strip "${dir}/${file}.txt" | grep -A1 -E '^[A-Z]+ +swtpm' || true
    done
    if [[ $installed == yes ]]; then
        check "$drill" "no config: PASS with version" "${dir}/no-config.txt" '^PASS +swtpm +swtpm [0-9]+\.[0-9]+(\.[0-9]+)? at /'
        check "$drill" "backend auto: PASS" "${dir}/auto.txt" '^PASS +swtpm +swtpm [0-9.]+ at /'
        check "$drill" "backend swtpm: PASS" "${dir}/swtpm.txt" '^PASS +swtpm +swtpm [0-9.]+ at /'
    else
        check "$drill" "no config: WARN not found" "${dir}/no-config.txt" '^WARN +swtpm +not found'
        check "$drill" "no config: install fix" "${dir}/no-config.txt" 'brew install swtpm'
        check "$drill" "backend auto: WARN not found" "${dir}/auto.txt" '^WARN +swtpm +not found'
        check "$drill" "backend swtpm: FAIL not found" "${dir}/swtpm.txt" '^FAIL +swtpm +not found'
    fi
    check "$drill" "enabled = false: SKIP" "${dir}/disabled.txt" '^SKIP +swtpm +not used: \[tpm\] enabled = false'
}

drill_disabled() {
    local dir="${out_dir}/disabled" name=drill-tpm-off
    mkdir -p "$dir"
    prepare_vm "$dir"
    write_config "$dir" "$name" "${dir}/esp.img" 'enabled = false'
    printf 'tpm-drill: disabled\n'
    boot "$dir" "$name" || true
    finish "$dir" "$name"
    local samples with
    samples=$(wc -l <"${dir}/ps.txt" | tr -d ' ')
    with=$(grep -vc 'swtpm_children=0' "${dir}/ps.txt" || true)
    printf 'ps: %s samples, %s with a swtpm child of ternvale run (pid %s)\n' "$samples" "$with" "$run_pid" | tee "${dir}/ps-summary.txt"
    check disabled "ps sampled while the VM ran" "${dir}/ps-summary.txt" '^ps: ([2-9][0-9]|[1-9][0-9]{2,}) samples'
    check disabled "no swtpm child in any ps sample" "${dir}/ps-summary.txt" ', 0 with a swtpm child'
    check disabled "host log: tpm disabled" "${dir}/host.log" 'tpm disabled \(\[tpm\] enabled = false\): no tpm device, no swtpm'
    refute disabled "host log: swtpm never started" "${dir}/host.log" 'starting swtpm|swtpm ready'
    check disabled "host log: ACPI read-back lists 9 tables, no TPM2" "${dir}/host.log" 'acpi tables read back from guest memory .*tables=9 signatures=RSD PTR,XSDT,FACP,DSDT,APIC,GTDT,MCFG,SPCR,DBG2$'
    refute disabled "host log: no TPM2 signature anywhere" "${dir}/host.log" 'signatures=.*TPM2'
    check disabled "host log: ACPI check passed without a TPM" "${dir}/host.log" 'acpi tables match the platform map .*tpm=false tables=9'
    check disabled "host log: guest DTB has no TPM node" "${dir}/host.log" 'built guest dtb .*tpm=false'
    check disabled "guest: no TPM2 table, no tpmrm0" "${dir}/guest-serial.log" 'ternvale-tpm: probe acpi_tpm2=n tpmrm0=n'
}

drill_kill() {
    local dir="${out_dir}/kill" name=drill-tpm-kill
    mkdir -p "$dir"
    prepare_vm "$dir"
    write_config "$dir" "$name" "${dir}/esp.img" 'backend = "swtpm"'
    printf 'tpm-drill: kill\n'
    boot "$dir" "$name" || true
    local swtpm_pid=""
    if wait_for 120 "${dir}/guest-serial.log" "EFI stub: Booting Linux Kernel"; then
        swtpm_pid=$(pgrep -P "$run_pid" swtpm || true)
    fi
    if [[ -n "$swtpm_pid" ]]; then
        kill -KILL "$swtpm_pid"
        printf 'killed swtpm pid %s (child of ternvale run pid %s) at the EFI stub\n' "$swtpm_pid" "$run_pid" | tee "${dir}/killed.txt"
    else
        printf 'no swtpm child of ternvale run pid %s found\n' "$run_pid" | tee "${dir}/killed.txt"
    fi
    finish "$dir" "$name"
    set +e
    "$ternvale" logs "$name" --level error >"${dir}/logs-error.txt" 2>&1
    "$ternvale" report "$name" --out "${dir}/bundle.zip" >"${dir}/report.txt" 2>&1
    set -e
    mkdir -p "${dir}/bundle"
    unzip -q -o -j "${dir}/bundle.zip" -d "${dir}/bundle" 2>/dev/null || true
    printf -- '--- ternvale logs %s --level error ---\n' "$name"
    strip "${dir}/logs-error.txt" | grep 'ternvale::tpm' | cut -c1-400 || true
    printf -- '--- bundle tpm-commands.txt (header and last 6) ---\n'
    grep '^#' "${dir}/bundle/tpm-commands.txt" 2>/dev/null || true
    tail -6 "${dir}/bundle/tpm-commands.txt" 2>/dev/null | cut -c1-200 || true
    check kill "swtpm was a child of ternvale run, and was killed" "${dir}/killed.txt" '^killed swtpm pid [0-9]+'
    check kill "ps saw the swtpm child before the kill" "${dir}/ps.txt" 'swtpm_children=1'
    check kill "ERROR at once: swtpm exited while the VM ran" "${dir}/logs-error.txt" 'ERROR .*ternvale::tpm: swtpm exited while the VM was running \(killed or crashed\)'
    check kill "ERROR names the failed command" "${dir}/logs-error.txt" 'ERROR .*ternvale::tpm: swtpm stopped answering; this TPM command and every later one get TPM_RC_FAILURE .*code=0x[0-9a-f]+ name="?[A-Za-z_]+'
    check kill "ERROR names swtpm's exit signal" "${dir}/logs-error.txt" 'swtpm stopped answering.*swtpm=swtpm \(pid [0-9]+\) exited with signal: 9 \(SIGKILL\)'
    refute kill "no ERROR for CMD_SHUTDOWN to the dead swtpm" "${dir}/logs-error.txt" 'CMD_SHUTDOWN'
    check kill "bundle has tpm-commands.txt" "${dir}/report.txt" 'tpm-commands\.txt: [0-9]+ bytes'
    check kill "bundle README opens with the TPM failure" "${dir}/report.txt" '^tpm: backend failed: swtpm \(pid [0-9]+\) exited with signal: 9 \(SIGKILL\)'
    check kill "history header names the failure" "${dir}/bundle/tpm-commands.txt" '^# backend failed: swtpm \(pid [0-9]+\) exited with signal: 9 \(SIGKILL\): .*, during [A-Za-z_]+ \(0x[0-9a-f]+\); seq=[0-9]+ and every later command got TPM_RC_FAILURE'
    check kill "history: commands before the kill answered" "${dir}/bundle/tpm-commands.txt" '^seq=[0-9]+ .* rc=0x0 '
    check kill "history: failed commands answered TPM_RC_FAILURE" "${dir}/bundle/tpm-commands.txt" '^seq=[0-9]+ .* rc=0x101 '
    check kill "guest: VM kept running and powered off" "${dir}/guest-serial.log" 'ternvale-tpm: done'
}

for drill in $drills; do
    case "$drill" in
        doctor) drill_doctor ;;
        disabled) drill_disabled ;;
        kill) drill_kill ;;
        *)
            printf 'tpm-drill: unknown drill %s\n' "$drill" >&2
            exit 2
            ;;
    esac
done

printf '\n%s check(s) failed; outputs in %s\n' "$failed" "${out_dir#"${root}/"}" >>"$results"
cat "$results"
if ((failed)); then
    exit 1
fi
