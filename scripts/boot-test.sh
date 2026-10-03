#!/bin/bash
# Boot the test kernel and match serial output. Exit 0 only on a clean SYSTEM_OFF.
# Logs and the guest serial transcript land in target/boot-logs/<timestamp>/.
#
# Scenarios (TERNVALE_BOOT_SCENARIO):
#   initrd  (default) — busybox initramfs smoke test
#   rootfs            — virtio-blk ext4 persistence (needs ./scripts/make-rootfs.sh),
#                       then a host `fsck.ext4 -fn` of the image via scripts/fsck-rootfs.sh
#   pci               — the rootfs scenario with the disk on virtio-pci (00:01.0) behind
#                       the ECAM host bridge, plus `lspci -nn` and a hand mount of
#                       data.ext4 on 00:02.0 (needs ./scripts/make-pci-assets.sh);
#                       host fsck of both images
#   net               — virtio-net loopback ping (needs ./scripts/make-net-initramfs.sh),
#                       then tshark filters over net.pcap via scripts/check-pcap.sh
#   devices           — virtio-rng /dev/hwrng samples and a vsock ping/pong on port 5000
#                       (needs ./scripts/make-devices-initramfs.sh)
#   agent             — the guest ternvale-agent (same assets) against the host AgentServer:
#                       handshake, heartbeats, reconnect after a server restart, clipboard /
#                       resolution requests, and power-off on Shutdown
#   smp               — nproc, /proc/cpuinfo, and one dd per CPU checked in /proc/stat
#                       and top, on TERNVALE_BOOT_CPUS CPUs (default 4)
#   firmware          — EDK2 UEFI (needs ./scripts/fetch-firmware.sh): UEFI shell, front
#                       page, Boot Manager, and an NV variable that survives a second boot
#                       on the same nvram.fd (firmware_tables = "fdt": DTB, no ACPI)
#   firmware-acpi     — EDK2 with the default firmware_tables = "acpi": tables reach EDK2
#                       over fw_cfg; the UEFI shell's dmem walks what EDK2 installed and the
#                       harness compares it with the builder (acpi-tables.txt)
#   firmware-debug    — verbose DEBUG EDK2 (needs ./scripts/fetch-debug-firmware.sh) with
#                       firmware_tables = "acpi" up to BDS; EDK2's own serial log must show
#                       it installed the fw_cfg ACPI tables and did not expose the DTB
#   uefi-linux        — the test kernel and initramfs booted through EDK2 (firmware_tables =
#                       "fdt") from a FAT16 disk; initrd checks plus /sys/firmware/{efi,fdt}
#                       present and /sys/firmware/acpi absent, then poweroff
#   TERNVALE_FIRMWARE=<QEMU_EFI.fd> replaces the EDK2 image in the UEFI scenarios.
#   installer         — EDK2 boots the Alpine arm64 ISO (needs ./scripts/fetch-installer-iso.sh)
#                       from a read-only virtio-blk disk to a root login and setup-alpine;
#                       TERNVALE_INSTALLER_TRANSPORT=pci puts the disk on virtio-pci.
#                       ./scripts/qemu-compare.sh runs the same steps under QEMU/HVF
# scripts/boot-stress.sh repeats this script N times and summarizes the runs.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
cd "$root"

stamp=$(date +%Y%m%d-%H%M%S)
scenario="${TERNVALE_BOOT_SCENARIO:-initrd}"
log_dir="${root}/target/boot-logs/${stamp}-${scenario}"
mkdir -p "$log_dir"
export TERNVALE_BOOT_LOG_DIR="$log_dir"
export TERNVALE_BOOT_SCENARIO="$scenario"

printf 'boot-test: scenario %s logs %s\n' "$scenario" "$log_dir"
set +e
cargo test -p ternvale-devices --features boot-test --test boot -- --nocapture
status=$?
set -e
if [[ ( "$scenario" == "rootfs" || "$scenario" == "pci" ) && "$status" -eq 0 ]]; then
    set +e
    "${script_dir}/fsck-rootfs.sh" "${log_dir}/rootfs.ext4" /root/t persist
    status=$?
    set -e
fi
if [[ "$scenario" == "pci" && "$status" -eq 0 ]]; then
    set +e
    "${script_dir}/fsck-rootfs.sh" "${log_dir}/data.ext4" /m pci-data
    status=$?
    set -e
fi
if [[ "$scenario" == "net" && "$status" -eq 0 ]]; then
    set +e
    "${script_dir}/check-pcap.sh" "${log_dir}/net.pcap"
    status=$?
    set -e
fi
printf 'boot-test: exit %s\n' "$status"
printf 'boot-test: artifacts %s\n' "$log_dir"
exit "$status"
