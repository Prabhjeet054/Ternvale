#!/usr/bin/env bash
# Copy test-assets/desktop into the app VM library as "desktop".
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
assets="$root/test-assets/desktop"
lib="${HOME}/Library/Application Support/Ternvale/vms/desktop"
name="${VM_NAME:-desktop}"

[[ -f "$assets/vm.toml.example" ]] || { echo "run scripts/make-desktop-rootfs.sh first" >&2; exit 1; }

mkdir -p "$lib/disks"
for f in Image initramfs.cpio rootfs.ext4; do
    ln -sf "$assets/$f" "$lib/$f" 2>/dev/null || cp -c "$assets/$f" "$lib/$f"
done
cat >"$lib/config.toml" <<EOF
name = "$name"
cpus = 2
ram_mib = 1024
kernel = "$lib/Image"
initrd = "$lib/initramfs.cpio"
serial_log = "$lib/serial.log"
cmdline = "console=tty0 console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init"

[[disks]]
path = "$lib/rootfs.ext4"
read_only = false
EOF
echo "Installed desktop VM at $lib (open Ternvale app and start \"$name\")"
