# Snapshots

The Mac app can **Pause** / **Resume** a VM. It has **no** snapshot buttons.
Use the CLI while the VM is running.

Full-VM snapshots capture guest RAM, GIC, and vCPU registers. **Pause before
restore.** Create also talks to a live control socket (the VM must be up).

```sh
export PATH="/Applications/Ternvale.app/Contents/Helpers:$PATH"
ternvale pause myvm
ternvale snapshot create myvm before-update
ternvale snapshot list myvm
ternvale snapshot restore myvm before-update
ternvale resume myvm
ternvale snapshot delete myvm before-update
```

Linked clones (`ternvale clone src dst`) share the source disk read-only and
write a new overlay. Templates:
`ternvale template create <vm> <template>` then
`ternvale new <new-vm> --from-template <template>`.

Portable bundles: `ternvale export myvm ./myvm.tvm` and `ternvale import`.

Snapshot files on disk use a versioned format (magic + version). Unknown
versions are refused rather than guessed.
