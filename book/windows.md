# Creating Windows VMs

**You must supply a Windows 11 ARM64 installer ISO and a valid licence.**
Ternvale never generates or ships a Windows product key. Canonical text:
`docs/WINDOWS_LICENSE.md` in the source tree.

Interactive Setup is **not** wired. `ternvale create --windows` **requires**
`--unattended`.

## Firmware and tools (required on a clean Mac)

Windows is UEFI. Before create:

```sh
# AAVMF / EDK2 (BSD-2-Clause-Patent). Not inside Ternvale.app.
./scripts/fetch-firmware.sh
export TERNVALE_FIRMWARE="$PWD/test-assets/firmware/QEMU_EFI.fd"
# or: ternvale create --windows --firmware /path/to/QEMU_EFI.fd …

# virtio-win + unmodified WinFsp MSI (not linked into the Mac app)
ternvale tools build-iso --fetch
```

Without `--firmware` or `TERNVALE_FIRMWARE`, create fails unless a developer
tree has `test-assets/firmware/QEMU_EFI.fd`.

## CLI

```sh
export TERNVALE_WIN_PASSWORD='choose-a-local-password'
ternvale create --windows --name win11 \
  --iso /path/to/Windows11_ARM64.iso \
  --unattended \
  --password-env TERNVALE_WIN_PASSWORD \
  --wait
```

This writes a library VM, a 64G disk, Autounattend ISO, user-mode NIC, and
`[vsock] agent = true`. Default local account is `Ternvale`. Optional
`--product-key-env` / `--product-key-file` pass **your** key; they are never
logged. `--wait` can run for hours (`--timeout-secs`, default 7200).

Need **at least two vCPUs** (the Windows profile sets `cpus = 2`).

## In the app

**New VM** → enable **Windows 11 (unattended)** → **Windows ISO path** and
**Account password** (stored in Keychain; also passed to the helper via
`TERNVALE_WIN_PASSWORD`). Product key is optional.

The app calls the same `ternvale create --windows --unattended` helper. Set
`TERNVALE_FIRMWARE` in your login environment (or place firmware where the
CLI can find it) **before** you click Create, and have a tools ISO
(`ternvale tools build-iso --fetch`). If Setup cannot see the disk, load
`viostor` from the tools / virtio-win ISO.
