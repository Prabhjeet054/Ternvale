#!/bin/bash
# Break a working VM three ways and check that `ternvale doctor`, `ternvale logs`
# and the crash bundle each name the cause, within one minute of starting the VM.
#
#   ./scripts/fault-drill.sh            # all faults
#   ./scripts/fault-drill.sh disk       # one of: kernel disk cmdline
#
# Faults, all on the virtio-root assets (./scripts/make-rootfs.sh):
#   kernel  — `kernel` points at a file that does not exist
#   disk    — the root disk's ext4 superblock is zeroed (bytes 1024..2047)
#   cmdline — `console=ttyS0`, a console this VM does not have
# The bundle is checked through its README.txt, which opens with doctor's FAIL
# lines. Each fault's outputs go to target/fault-drill/<stamp>/<fault>/: run.stderr,
# doctor.txt, logs.txt, the bundle zip and its unpacked files. results.md has
# one row per fault. Exits 1 if any tool missed the cause or a fault took
# longer than LIMIT seconds.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

LIMIT=60
faults="${*:-kernel disk cmdline}"
assets="${root}/test-assets/virtio-root"
for file in Image initramfs.cpio rootfs.ext4; do
    if [[ ! -f "${assets}/${file}" ]]; then
        printf 'fault-drill: missing %s (run ./scripts/make-rootfs.sh)\n' "${assets}/${file}" >&2
        exit 2
    fi
done
out_dir="${root}/target/fault-drill/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out_dir"
results="${out_dir}/results.md"

cargo build -q -p ternvale-cli
ternvale="${CARGO_TARGET_DIR:-${root}/target}/debug/ternvale"
codesign --sign - --force --entitlements entitlements/ternvale.entitlements "$ternvale" 2>/dev/null

strip() { sed 's/\x1b\[[0-9;]*m//g' "$@"; }

# write_config <name> <kernel> <cmdline> <disk>
write_config() {
    cat >"${dir}/vm.toml" <<EOF
name = "$1"
cpus = 1
ram_mib = 256
kernel = "$2"
initrd = "${assets}/initramfs.cpio"
serial_log = "${dir}/guest-serial.log"
boot_disk = true
cmdline = "$3"

[[disks]]
path = "$4"
read_only = false
EOF
}

# wait_for <seconds> <file> <text>: 0 once file contains text.
wait_for() {
    local deadline=$((SECONDS + $1))
    while ((SECONDS < deadline)); do
        if [[ -f "$2" ]] && grep -q "$3" "$2"; then
            return 0
        fi
        sleep 0.5
    done
    return 1
}

# found <label> <file> <regex>: record whether a tool's output names the cause.
found() {
    if strip "$2" 2>/dev/null | grep -Eq "$3"; then
        printf 'fault-drill:   %-7s names the cause: %s\n' "$1" \
            "$(strip "$2" | grep -Eo "$3" | head -1)"
        verdict+=("yes")
    else
        printf 'fault-drill:   %-7s MISSED the cause (/%s/ not in %s)\n' "$1" "$3" "$2"
        verdict+=("NO")
        missed=1
    fi
}

printf '| Fault | Symptom | doctor --config | logs --level warn | bundle | Seconds |\n|---|---|---|---|---|---|\n' >"$results"
failed=0
for fault in $faults; do
    dir="${out_dir}/${fault}"
    mkdir -p "$dir"
    name="drill-${fault}"
    disk="${dir}/rootfs.ext4"
    cp "${assets}/rootfs.ext4" "$disk"
    kernel="${assets}/Image"
    cmdline=""
    case "$fault" in
        kernel) kernel="${dir}/missing/Image" ;;
        disk) dd if=/dev/zero of="$disk" bs=1024 seek=1 count=1 conv=notrunc 2>/dev/null ;;
        cmdline) cmdline="console=ttyS0" ;;
        *)
            printf 'fault-drill: unknown fault %s\n' "$fault" >&2
            exit 2
            ;;
    esac
    write_config "$name" "$kernel" "$cmdline" "$disk"
    printf 'fault-drill: %s\n' "$fault"
    started=$SECONDS
    verdict=()
    missed=0

    # The guest's stdin is a FIFO this script holds open, as a terminal would be.
    mkfifo "${dir}/stdin"
    "$ternvale" run "${dir}/vm.toml" <"${dir}/stdin" >"${dir}/run.stdout" 2>"${dir}/run.stderr" &
    run_pid=$!
    exec 3>"${dir}/stdin"
    case "$fault" in
        kernel)
            wait_for 10 "${dir}/run.stderr" "error:" || true
            symptom="run: $(strip "${dir}/run.stderr" | grep -m1 '^error:' | sed "s#${dir}/##g" || true)"
            ;;
        disk)
            wait_for 30 "${dir}/guest-serial.log" "mount /dev/vda failed" || true
            symptom="serial: $(grep -m1 'mount /dev/vda failed' "${dir}/guest-serial.log" || echo 'no mount error yet')"
            ;;
        cmdline)
            sleep 10
            symptom="serial log has $(wc -c <"${dir}/guest-serial.log" | tr -d ' ') bytes after 10 s"
            ;;
    esac

    set +e
    "$ternvale" doctor --config "${dir}/vm.toml" >"${dir}/doctor.txt" 2>"${dir}/doctor.stderr"
    "$ternvale" logs "$name" --level warn >"${dir}/logs.txt" 2>&1
    "$ternvale" report "$name" --out "${dir}/bundle.zip" >"${dir}/report.txt" 2>&1
    set -e
    mkdir -p "${dir}/bundle"
    unzip -q -o -j "${dir}/bundle.zip" -d "${dir}/bundle" 2>/dev/null || true
    seconds=$((SECONDS - started))

    case "$fault" in
        kernel)
            found doctor "${dir}/doctor.txt" 'FAIL +kernel +[^ ]*missing/Image: No such file or directory'
            found logs "${dir}/logs.txt" 'vm config rejected; not booting .*kernel is not an existing file: [^ ]*missing/Image'
            found bundle "${dir}/bundle/README.txt" 'doctor: FAIL +kernel +[^ ]*missing/Image: No such file or directory'
            ;;
        disk)
            found doctor "${dir}/doctor.txt" 'FAIL +disk 0 .*has no ext2/3/4 superblock'
            found logs "${dir}/logs.txt" 'vm config problem check=disk 0 .*no ext2/3/4 superblock'
            found bundle "${dir}/bundle/README.txt" 'doctor: FAIL +disk 0 .*has no ext2/3/4 superblock'
            ;;
        cmdline)
            found doctor "${dir}/doctor.txt" 'FAIL +cmdline +console=ttyS0 is not a device on this VM'
            found logs "${dir}/logs.txt" 'vm config problem check=cmdline .*console=ttyS0 is not a device'
            found bundle "${dir}/bundle/README.txt" 'doctor: FAIL +cmdline +console=ttyS0 is not a device on this VM'
            ;;
    esac

    "$ternvale" stop "$name" >/dev/null 2>&1 || true
    exec 3>&-
    wait "$run_pid" || true
    if ((seconds > LIMIT)); then
        printf 'fault-drill:   took %ss, over the %ss limit\n' "$seconds" "$LIMIT"
        missed=1
    fi
    if ((missed)); then
        failed=$((failed + 1))
    fi
    printf '| %s | %s | %s | %s | %s | %s |\n' "$fault" "${symptom//|/\\|}" \
        "${verdict[0]}" "${verdict[1]}" "${verdict[2]}" "$seconds" >>"$results"
done

printf '\n%s fault(s) not diagnosed within %ss; outputs in %s\n' \
    "$failed" "$LIMIT" "${out_dir#"${root}/"}" >>"$results"
cat "$results"
if ((failed)); then
    exit 1
fi
