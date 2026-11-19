# tvgpu-dod — Ternvale virtio-gpu display-only (KMDOD)

Windows 11 ARM64 **WDDM display-only** miniport for Ternvale’s virtio-gpu
device (2D protocol + EDID + `VIRTIO_GPU_EVENT_DISPLAY`). Built with Visual
Studio + WDK **inside** the guest. Independent implementation of OASIS virtio
1.2 §5.7 and Microsoft KMDOD DDIs. Microsoft’s KMDOD sample and virtio-win
`viogpudo` were **behavioral references only** (no source copied). See
`THIRD_PARTY_NOTICES.md`.

## Hardware ID

`PCI\VEN_1AF4&DEV_1050` (virtio-pci `0x1040 + 16`), subsystem `1AF4:1100`.

## Present path

`SetVidPnSourceAddress` → `RESOURCE_CREATE_2D` + `ATTACH_BACKING` +
`TRANSFER_TO_HOST_2D` + `SET_SCANOUT` + `RESOURCE_FLUSH` on `controlq`.

Host values (`num_scanouts`, mode width/height, EDID size, response type,
scanout index) are range-checked before use.

## Cursor limitation

The device exposes `cursorq`. Ternvale’s host **acknowledges**
`UPDATE_CURSOR` / `MOVE_CURSOR` with `RESP_OK_NODATA` but **does not
composite** a hardware cursor (see `ternvale-gpu` worker). This driver
therefore reports **software cursor** (`PointerCaps` empty) so Windows draws
the pointer. Hardware cursor over `cursorq` is compiled in but off unless
host compositing exists.

## Build / install

```powershell
.\drivers\build.ps1 -Project (Resolve-Path .\drivers\tvgpu-dod\tvgpu-dod.vcxproj)
bcdedit /set testsigning on
pnputil /add-driver drivers\tvgpu-dod\tvgpu-dod.inf /install
```

Sidecar: `C:\ProgramData\Ternvale\driver-logs\tvgpu-dod.log`.
WPP/ETW GUID `{A9C3E271-4B18-4D6F-8E01-2C5A7B9D0E11}`.

Guest live script: `drivers/scripts/tvgpu-dod-live.ps1` (Device Manager, Code
Analysis, SDV when present, Driver Verifier, disable/enable, optional 1h
stress). Host analog: `./scripts/tvgpu-dod-live-check.sh`.
