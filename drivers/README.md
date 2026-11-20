# Ternvale Windows guest drivers

KMDF drivers built **inside** a Windows 11 ARM64 VM (Visual Studio + WDK).
This tree is not part of the Rust Cargo workspace.

See [docs/DRIVER_DEV.md](../docs/DRIVER_DEV.md) for the driver-dev VM, test-signing,
kernel debugging, Driver Verifier, crash dumps, and pulling logs with
`ternvale driver-logs`.

## Layout

| Path | Role |
|---|---|
| `Directory.Build.props` | Warnings-as-errors, Code Analysis |
| `build.ps1` | Shared ARM64 MSBuild wrapper |
| `tvhello/` | Minimal KMDF “hello” (WPP/ETW + device interface + sidecar log) |
| `tvgpu-dod/` | virtio-gpu **KMDOD** display-only WDDM miniport (2D + EDID) |
| `tvvk/` | Prototype **user-mode Vulkan ICD** (`tvvk.dll`). Not a render KMD. Step 108. |

## Build (on Windows ARM64)

```powershell
# From the repo root, inside the guest
.\drivers\build.ps1
.\drivers\build.ps1 -Config Release
.\drivers\build.ps1 -Project .\drivers\tvgpu-dod\tvgpu-dod.vcxproj
.\drivers\build.ps1 -Project .\drivers\tvvk\tvvk.vcxproj   # user-mode v143 DLL
```

Signing keys and certificates stay in the guest keychain / CI secret store.
Never commit `.pfx`, passwords, or Microsoft-signed catalogs.

## Install (test-signed)

```powershell
bcdedit /set testsigning on   # reboot once
pnputil /add-driver drivers\tvhello\tvhello.inf /install
pnputil /add-driver drivers\tvgpu-dod\tvgpu-dod.inf /install
pnputil /add-driver drivers\tvvk\tvvk.inf /install   # prototype ICD; fail-closed
```

Sidecar text: `C:\ProgramData\Ternvale\driver-logs\*.log`.
The in-guest agent concatenates `*.log` / `*.txt` there for the host.

`tvgpu-dod` is display-only; Windows draws a **software cursor** because the
host acknowledges `cursorq` without compositing. See `docs/TVGPU_DOD.md`.
`tvvk` is a **user-mode** ICD (`v143` Desktop DLL, not KMDF). 3D probe failure
leaves KMDOD as the only display path (`docs/GPU_ACCELERATION.md` Step 108).
Guest live: `.\drivers\scripts\tvgpu-dod-live.ps1`. Host:
`./scripts/tvgpu-dod-live-check.sh`.

Packaging / test-sign / Partner Center attestation: `drivers/release/`
(`docs/SECURE_BOOT.md` for UEFI Secure Boot gaps).
