#!/bin/bash
# Boot the same firmware + installer ISO under QEMU/HVF as the `installer` boot scenario, so
# the serial logs and guest diagnostics can be diffed (see docs/DEBUGGING.md, "Installer ISO
# under UEFI: Ternvale vs QEMU").
#
#   qemu-system-aarch64 -M virt -accel hvf -bios test-assets/firmware/QEMU_EFI.fd ...
#
# Needs `qemu-system-aarch64` (brew install qemu), `expect` (macOS ships /usr/bin/expect),
# and `dtc` for the DTB dump. Artifacts go to target/qemu-logs/<stamp>/:
#   serial.log  raw guest serial (VT100), same steps as tests/boot/installer.rs
#   virt.dts    the device tree QEMU generates for this command line (dumpdtb)
#   cmdline.txt the exact QEMU command line
#
# Usage:
#   ./scripts/qemu-compare.sh                  # ISO on virtio-blk-device (virtio-mmio)
#   QEMU_TRANSPORT=pci ./scripts/qemu-compare.sh
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
fw="${QEMU_FIRMWARE:-${root}/test-assets/firmware/QEMU_EFI.fd}"
iso="${QEMU_ISO:-${root}/test-assets/installer/installer.iso}"
transport="${QEMU_TRANSPORT:-mmio}"
out="${root}/target/qemu-logs/$(date +%Y%m%d-%H%M%S)-${transport}"

log() {
    printf 'qemu-compare: %s\n' "$*" >&2
}

for f in "${fw}" "${iso}"; do
    [[ -f "${f}" ]] || { log "missing ${f} (fetch-firmware.sh / fetch-installer-iso.sh)"; exit 1; }
done
command -v qemu-system-aarch64 >/dev/null || { log "qemu-system-aarch64 not found (brew install qemu)"; exit 1; }
case "${transport}" in
    mmio) blk=virtio-blk-device ;;
    pci) blk=virtio-blk-pci ;;
    *) log "QEMU_TRANSPORT=${transport} (expected mmio or pci)"; exit 1 ;;
esac
mkdir -p "${out}"

machine=(-accel hvf -cpu host -smp 1 -m 1024 -bios "${fw}"
    -drive "if=none,id=iso,format=raw,readonly=on,file=${iso}" -device "${blk},drive=iso")
args=(-M virt "${machine[@]}" -nographic -no-reboot)
printf '%q ' qemu-system-aarch64 "${args[@]}" > "${out}/cmdline.txt"
echo >> "${out}/cmdline.txt"
log "$(cat "${out}/cmdline.txt")"

qemu-system-aarch64 -M "virt,dumpdtb=${out}/virt.dtb" "${machine[@]}" -nographic 2>/dev/null
if command -v dtc >/dev/null; then
    dtc -q -I dtb -O dts -o "${out}/virt.dts" "${out}/virt.dtb"
fi

driver=$(mktemp -t qemu-compare)
trap 'rm -f "${driver}"' EXIT
cat > "${driver}" <<'EXPECT'
set serial [lindex $argv 0]
set qargs [lrange $argv 1 end]
log_user 0
log_file -a -noappend $serial
proc fail {what} {
    puts stderr "qemu-compare: timed out waiting for $what"
    exit 1
}
set t0 [clock milliseconds]
proc want {pattern secs} {
    global t0
    set timeout $secs
    expect {
        -ex $pattern {}
        timeout { fail $pattern }
        eof { fail "$pattern (qemu exited)" }
    }
    puts stderr "qemu-compare: matched elapsed_ms=[expr {[clock milliseconds] - $t0}] pattern=$pattern"
}
proc run {cmd n} {
    send -- "$cmd\r"
    want "TV-$n" 30
}
spawn qemu-system-aarch64 {*}$qargs
want "UEFI firmware" 60
want "UEFI Misc Device" 60
want "GNU GRUB" 60
want "Booting `Linux virt'" 30
want "OpenRC" 120
want "Welcome to Alpine Linux" 300
want "login:" 60
send "root\r"
want "localhost:~#" 60
run {dmesg | grep -iE 'efi|dmi|smbios|acpi|rtc|psci|rng|magic|fail|error|warn|Machine'; echo TV-$((0+1))} 1
run {ls /sys/firmware /sys/firmware/efi; echo TV-$((1+1))} 2
run {ls /sys/firmware/efi/efivars | cut -d- -f1 | sort | tr '\n' ' '; echo TV-$((2+1))} 3
run {ls /sys/bus/platform/devices; echo TV-$((3+1))} 4
run {cat /proc/interrupts; blkid; echo TV-$((4+1))} 5
run {base64 /sys/firmware/fdt; echo TV-$((5+1))} 6
send "setup-alpine\r"
want "ALPINE LINUX INSTALL" 60
want "Enter system hostname" 60
send "\003"
want "localhost:~#" 30
send "poweroff\r"
set timeout 60
expect eof
EXPECT
started=$(date +%s)
expect -f "${driver}" -- "${out}/serial.log" "${args[@]}"
# The DT EDK2 handed to Linux, between the `base64` echo and the TV-6 marker.
tr -d '\r' < "${out}/serial.log" |
    awk '/base64 \/sys\/firmware\/fdt/ {on=1; next} /^TV-6$/ {on=0} on && /^[A-Za-z0-9+\/=]+$/' \
    > "${out}/guest-fdt.b64"
if command -v dtc >/dev/null; then
    base64 -D -i "${out}/guest-fdt.b64" | dtc -q -I dtb -O dts -o "${out}/guest-fdt.dts" -
fi
log "passed in $(( $(date +%s) - started )) s; artifacts ${out}"
