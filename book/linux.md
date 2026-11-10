# Creating Linux VMs

You supply the guest. Ternvale does not ship Alpine, Ubuntu, or any kernel.

Linux guests boot either:

- **Direct kernel**: `kernel` + optional `initrd` + `cmdline` in the TOML
- **UEFI**: `firmware` pointing at an ARM64 `QEMU_EFI.fd` (AAVMF / EDK2; fetch
  with `scripts/fetch-firmware.sh` or set `TERNVALE_FIRMWARE`) plus a disk
  with an ESP / installer

`test-assets/Image` is a **developer** payload. A clean Mac does not have it.

Example (direct kernel, user-mode NAT, one disk):

```toml
name = "alpine"
cpus = 2
ram_mib = 1024
kernel = "/path/to/Image"          # ARM64 Linux you downloaded
initrd = "/path/to/initramfs"      # optional
cmdline = "console=ttyAMA0 root=/dev/vda"

[[disks]]
path = "/Users/you/VMs/alpine.img"
cache = "writeback"

[[nics]]
backend = "user"
```

```sh
ternvale create-disk /Users/you/VMs/alpine.img 8G
ternvale validate alpine.toml
ternvale run alpine.toml
```

The Mac app’s **New VM** sheet writes the same library layout. It only fills
`kernel` (optional) and a `disk0.img` **path**; you still create the image and
edit kernel/firmware before **Start**.

## What is supported

ARM64 guests. Distros exercised in CI when images exist:
`docs/COMPAT.md` / `make compat` (Alpine, Debian, Ubuntu Server, Fedora,
Arch Linux ARM, FreeBSD). Missing images are **skips**, not passes.

There is **no** 32-bit ARM and **no** x86_64 guest.
