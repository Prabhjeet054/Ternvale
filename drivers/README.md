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

## Build (on Windows ARM64)

```powershell
# From the repo root, inside the guest
.\drivers\build.ps1
.\drivers\build.ps1 -Config Release
```

Signing keys and certificates stay in the guest keychain / CI secret store.
Never commit `.pfx`, passwords, or Microsoft-signed catalogs.

## Install (test-signed)

```powershell
bcdedit /set testsigning on   # reboot once
pnputil /add-driver drivers\tvhello\tvhello.inf /install
```

Sidecar text: `C:\ProgramData\Ternvale\driver-logs\tvhello.log`.
The in-guest agent concatenates `*.log` / `*.txt` there for the host.
