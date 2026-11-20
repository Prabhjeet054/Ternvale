# tvvk — Ternvale prototype Vulkan ICD (Windows ARM64)

User-mode DLL, **not** a WDDM render miniport. Independent code (not Mesa,
Yttrium, or gfxstream). Research and gaps: `docs/GPU_ACCELERATION.md`
Step 108.

## What it does

- Implements the Vulkan loader ICD ABI (`vk_icdNegotiateLoaderICDInterfaceVersion`,
  `vk_icdGetInstanceProcAddr`).
- Advertises **zero** physical devices when 3D probe fails (default): no
  `\\.\TVGPU3D` node, `TERNVALE_GPU3D=0`, or `HKLM\SOFTWARE\Ternvale\Gpu3d\Enabled=0`.
  The loader then skips this ICD; **tvgpu-dod** keeps 2D scanout.
- Encodes a length-prefixed nop CS (`encode.c`) matching `ternvale-gpu3d-host`.
- `vkCreateWin32SurfaceKHR` returns `VK_ERROR_FEATURE_NOT_PRESENT`. **vkcube.exe
  cannot present.**

## Build (Windows 11 ARM64, Visual Studio C++ ARM64 — not WDK kernel)

```powershell
.\drivers\build.ps1 -Project .\drivers\tvvk\tvvk.vcxproj
```

Install (test-signed, after tvgpu-dod): copy `tvvk.dll` + `tvvk.json` to
`%SystemRoot%\System32` and set

`HKLM\SOFTWARE\Khronos\Vulkan\Drivers` → `C:\Windows\System32\tvvk.json` = dword 0

or `pnputil /add-driver drivers\tvvk\tvvk.inf /install` (software-component INF;
prototype, not WHQL).

Sidecar log: `C:\ProgramData\Ternvale\driver-logs\tvvk.log`.

## Host check (macOS)

```sh
./scripts/win3d-proto-check.sh
```
