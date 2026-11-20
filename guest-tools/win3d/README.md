# Windows 3D prototype samples (Step 108)

**Prototype only.** These do not prove Metal-backed D3D11 in a Windows guest.
See `docs/GPU_ACCELERATION.md` Step 108.

| File | Role |
| --- | --- |
| `tvvk-smoke.c` | Load `tvvk.dll`, negotiate ICD, enumerate devices (expect **0** until a kernel 3D path exists), export nop CS |
| `d3d11_triangle.c` | Native D3D11 triangle (WARP or hardware). With **DXVK** `d3d11.dll` beside the exe it would target Vulkan |
| `fetch-dxvk.ps1` | Consent + download **DXVK LICENSE** (zlib) from GitHub; optional tarball. **Not** vendored |

DXVK official release DLLs are **x86/x64 Wine** layouts. Native **Windows ARM64**
DXVK is **not** an official artifact. TODO(verify) `dxvk-native` ARM64. Do not
drop x64 DXVK into an ARM64 guest and expect it to load.

MoltenVK stays on the **host** (Apache-2.0). It is never copied into the guest.

Kill switch: guest `TERNVALE_GPU3D=0` and host `[gpu] gpu3d = false`.

Guest live (Windows ARM64 SSH): `guest-tools/win3d/tvvk-live.ps1`. Host:
`./scripts/win3d-live-check.sh` (set `TERNVALE_WINDOWS_GUEST=1` to SSH).

Do not commit `third_party/` downloads.
