# Ternvale guest compatibility

Generated: 2026-11-10T13:33:03Z

Ternvale: `0.1.0` on `macOS-27.0.1-arm64-arm-64bit-Mach-O`.

Cells are pass / fail / skip. Skip means the guest does not support the feature (or the image was not present); it is not a failure.

| Guest | Kernel | Boot (s) | login | disk | net | display | input | audio | agent | share | shutdown |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Alpine Linux (`alpine-virt`) | virt | 0.5 | skip | skip | skip | skip | skip | skip | skip | skip | skip |
| Alpine Linux (`alpine-lts`) | lts | — | skip | skip | skip | skip | skip | skip | skip | skip | skip |
| Debian (`debian-aarch64`) | cloud | — | skip | skip | skip | skip | skip | skip | skip | skip | skip |
| Ubuntu Server (`ubuntu-server-aarch64`) | generic | — | skip | skip | skip | skip | skip | skip | skip | skip | skip |
| Fedora (`fedora-aarch64`) | generic | — | skip | skip | skip | skip | skip | skip | skip | skip | skip |
| Arch Linux ARM (`archlinuxarm`) | linux-aarch64 | — | skip | skip | skip | skip | skip | skip | skip | skip | skip |
| FreeBSD (`freebsd-arm64`) | GENERIC | — | skip | skip | skip | skip | skip | skip | skip | skip | skip |
| Windows 11 ARM (`windows11-arm`) | nt | — | skip | skip | skip | skip | skip | skip | skip | skip | skip |

## Notes

- `alpine-virt` skipped: host SIGKILL (AMFI/codesign) before guest serial; see docs/CI.md and scripts/sign-and-run.sh
- `alpine-lts` skipped: no image (kernel, initrd, disk) under test-assets/compat or env overlay
- `debian-aarch64` skipped: no image (disk) under test-assets/compat or env overlay
- `ubuntu-server-aarch64` skipped: no image (disk) under test-assets/compat or env overlay
- `fedora-aarch64` skipped: no image (disk) under test-assets/compat or env overlay
- `archlinuxarm` skipped: no image (disk) under test-assets/compat or env overlay
- `freebsd-arm64` skipped: no image (disk) under test-assets/compat or env overlay
- `windows11-arm` skipped: no image (disk or iso) under test-assets/compat or env overlay

## Review

Every **fail** must have a reason in Notes (or a tracking issue). Skip is not a failure: missing images, unsupported guest features, or the host killing the boot harness before serial (AMFI SIGKILL).

## Deliberate virtio-net disable

Same Alpine virt fixture with `COMPAT_BREAK=net` / virtio-net disabled. Net must be **fail**, not skip.

| Guest | login | net |
|---|---|---|
| Alpine Linux (`alpine-virt`) | pass | **fail** |

- net: COMPAT_BREAK=net (status=fail)
- login still pass
