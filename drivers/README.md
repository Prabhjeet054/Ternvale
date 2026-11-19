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

## Build (on Windows ARM64)

```powershell
# From the repo root, inside the guest
.\drivers\build.ps1
.\drivers\build.ps1 -Config Release
.\drivers\build.ps1 -Project .\drivers\tvgpu-dod\tvgpu-dod.vcxproj
```

Signing keys and certificates stay in the guest keychain / CI secret store.
Never commit `.pfx`, passwords, or Microsoft-signed catalogs.

## Install (test-signed)

```powershell
bcdedit /set testsigning on   # reboot once
pnputil /add-driver drivers\tvhello\tvhello.inf /install
pnputil /add-driver drivers\tvgpu-dod\tvgpu-dod.inf /install
```

Sidecar text: `C:\ProgramData\Ternvale\driver-logs\*.log`.
The in-guest agent concatenates `*.log` / `*.txt` there for the host.

`tvgpu-dod` is display-only; Windows draws a **software cursor** because the
host acknowledges `cursorq` without compositing. See `docs/TVGPU_DOD.md`.
Guest live: `.\drivers\scripts\tvgpu-dod-live.ps1`. Host:
`./scripts/tvgpu-dod-live-check.sh`.
