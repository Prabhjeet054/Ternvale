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
#   smp               — nproc, /proc/cpuinfo, and one dd per CPU checked in /proc/stat
#                       and top, on TERNVALE_BOOT_CPUS CPUs (default 4)
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
