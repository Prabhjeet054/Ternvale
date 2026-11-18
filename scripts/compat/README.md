# Guest compatibility matrix (Step 88)

`make compat` runs `scripts/compat/run.py --auto`:

- Boots each matrix row that has a disk/kernel under `test-assets/compat/`
  (Alpine virt can reuse `test-assets/virtio-root/`) when `kern.hv_support=1`.
- Guests without images are **skip**, not fail.
- Features a guest does not support (FreeBSD virtio-fs, Windows agent/share)
  are **skip**.
- A supported feature that is missing in serial, or a disabled virtio-net
  (`TERNVALE_COMPAT_NET=0`), is **fail**.
- Writes `docs/COMPAT.md` and `target/compat/compat.json`.

```sh
python3 -m unittest discover -s scripts/compat -p 'test_*.py' -v
python3 scripts/compat/run.py --offline
COMPAT_BREAK=net python3 scripts/compat/run.py --fixture-dir scripts/compat/fixtures
make compat   # --auto: live if HV is available
```

## x86 workloads (Step 100)

`make compat-x86` runs `scripts/compat/x86_run.py --auto`:

- Host `docker pull --platform linux/amd64` for nginx, postgres, redis, python,
  node, busybox, golang, Temurin when Docker Desktop is up (`--live-docker`).
- In-guest `docker run` and Windows x64 apps are **skip** without a full guest
  (same skip-not-fail rule as Step 88). Windows rows always note that
  **Windows on ARM** supplies x64 emulation; Ternvale does not control it.
- Merges a Step 100 section into `docs/X86_COMPAT.md` (keeps Step 98/99).
- Writes `target/compat/x86.json`.

```sh
python3 -m unittest discover -s scripts/compat -p 'test_x86.py' -v
python3 scripts/compat/x86_run.py --offline
make compat-x86
```

The app **Run x86 programs** toggle writes `x86_emulation = "auto"` and calls
`ternvale x86 plan|setup` (FEX RootFS, guest-setup, qemu-user).

Put distro disks at:

| Guest | Path |
|---|---|
| Alpine LTS | `test-assets/compat/alpine-lts/{Image,initramfs.cpio,rootfs.ext4}` |
| Debian | `test-assets/compat/debian/disk.qcow2` |
| Ubuntu Server | `test-assets/compat/ubuntu-server/disk.qcow2` |
| Fedora | `test-assets/compat/fedora/disk.qcow2` |
| Arch Linux ARM | `test-assets/compat/archlinuxarm/disk.qcow2` |
| FreeBSD | `test-assets/compat/freebsd/disk.qcow2` |
| Windows 11 ARM | `test-assets/compat/windows11/disk.vhdx` or a Win11 ISO |
