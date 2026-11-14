# FEX-Emu in a Ternvale Linux guest

FEX-Emu (MIT, Ryan Houdek 2019+) translates x86/x86_64 user binaries on
aarch64 Linux. Ternvale **does not bundle** FEX RootFS images (usually Ubuntu
from Canonical). Download those with `ternvale x86 fetch-rootfs` after typing
`yes`.

Rosetta (`VZLinuxRosettaDirectoryShare`) exists only on Apple’s
Virtualization.framework. The native Hypervisor.framework backend has no
equivalent; do not extract Apple’s translator.

## Host

```toml
backend = "native"   # or "auto"
profile = "linux"
x86_emulation = "fex"  # or "auto" (native → fex)
```

Build FEX for **aarch64 Linux** (not the Mac host):

```sh
./scripts/build-fex.sh            # needs Linux aarch64 or docker
./scripts/pack-linux-guest-tools.sh
```

Copy `target/linux-guest-tools/` into the guest (ISO overlay, virtio-fs share,
or a guest package repo you host). Never ship RootFS in that tarball.

## Guest

1. Install `FEXInterpreter` (and helpers) from the packed prefix.
2. Point FEX at the RootFS: copy the squashfs into the guest or share
   `~/Library/Application Support/Ternvale/fex/RootFS/` read-only.
3. Register binfmt:

```sh
sudo sh /usr/local/lib/ternvale/fex/register-binfmt.sh
```

TODO(verify) FEXInterpreter `--install-binfmt32` / `--install-binfmt64` flags
on the pinned FEX tag; the script writes systemd-binfmt files as a fallback.
