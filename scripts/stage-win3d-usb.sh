#!/usr/bin/env bash
# Copy Windows 3D sources onto the attached USB (default: /Volumes/UBUNTU 26_0).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
dest="${1:-/Volumes/UBUNTU 26_0}"
if [[ ! -d "$dest" ]]; then
  echo "USB not mounted at $dest" >&2
  exit 1
fi
out="$dest/Ternvale-win3d"
rm -rf "$out"
mkdir -p "$out/drivers" "$out/guest-tools"
rsync -a --exclude ARM64 --exclude .vs --exclude '*.obj' --exclude '*.pdb' \
  "$root/drivers/tvvk" "$root/drivers/tvgpu-dod" "$root/drivers/include" \
  "$out/drivers/"
cp "$root/drivers/build.ps1" "$out/drivers/"
rsync -a "$root/guest-tools/win3d" "$out/guest-tools/"
cat >"$out/README.txt" <<'EOF'
Ternvale Windows 3D (tvvk + tvgpu-dod IOCTL)
============================================
Copy this folder into the Windows 11 ARM64 VM (or build from it).

1. Install Visual Studio ARM64 + WDK.
2. From an elevated guest PowerShell:

   .\drivers\build.ps1 -Project .\drivers\tvgpu-dod\tvgpu-dod.vcxproj
   .\drivers\build.ps1 -Project .\drivers\tvvk\tvvk.vcxproj
   pnputil /add-driver drivers\tvgpu-dod\tvgpu-dod.inf /install
   pnputil /add-driver drivers\tvvk\tvvk.inf /install

3. Host VM must be started with a gpu-3d Ternvale build and [gpu] gpu3d = true
   so virtio-gpu offers VIRGL+CONTEXT_INIT (\\.\TVGPU3D ping returns 1).

4. vkcube.exe: install a Vulkan SDK ARM64 build, or use the LunarG vkcube.
   Present goes through tvvk WSI -> IOCTL SUBMIT_3D -> host MoltenVK.

5. DXVK: official tarballs are x64 Wine. Native ARM64 is not on this stick.
   Run guest-tools\win3d\fetch-dxvk.ps1 after typing yes (LICENSE only).

Kill switch: TERNVALE_GPU3D=0 or host [gpu] gpu3d = false (2D KMDOD remains).
EOF
echo "staged $out"
ls -la "$out" "$out/drivers" "$out/guest-tools"
