# Shared folders

There is **no** shared-folder control in the Mac app yet. Edit `config.toml`
(VM stopped) or pass the same keys to a CLI config.

Host directories are exported with **virtio-fs**. At most **one** `[[shares]]`
entry is accepted. The host path must already be a directory. `..` and
symlink escapes are rejected at the VMM.

Shares also require **`[vsock]` with `agent = true`** (validate fails
otherwise). Example:

```toml
[vsock]
agent = true

[[shares]]
host_path = "/Users/you/Share"
tag = "ternvale"
mount_point = "/mnt/ternvale"
read_only = false
```

```sh
mkdir -p /Users/you/Share
ternvale validate /path/to/config.toml
```

## Linux guest

```sh
mkdir -p /mnt/ternvale
mount -t virtiofs ternvale /mnt/ternvale
```

If the guest agent is running it can mount `mount_point` after handshake.

## Windows guest

Use `mount_point = "Z:"` (drive letter). Then:

1. Install **WinFsp** (unmodified MSI from the tools ISO).
2. Install virtio-win **viofs** and start `virtiofs.exe` / `VirtioFsSvc`.

WinFsp is GPLv3 with a FLOSS exception. Ternvale does not link it. See
`THIRD_PARTY_NOTICES.md`.
