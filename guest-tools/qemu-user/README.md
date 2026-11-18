# qemu-user-static (last-resort x86)

QEMU usermode (`qemu-x86_64-static`) is **GPL**. Ternvale never links it and
never ships a copy. Install the **guest distro** package after typing `yes` on
the Mac:

```sh
ternvale x86 install-qemu-user <vm>
```

That copies `install-qemu-user.sh` into the guest and runs `apt`/`dnf`/`apk`.
QEMU stays a **separate program** registered with `binfmt_misc`.

Engine order (see `docs/X86.md`): Rosetta, then FEX, then qemu-user.
Memory ordering: `docs/TSO.md`.
