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

v2 bundles compress RAM with zstd and can store only dirty pages on top of a
parent snapshot (`chain.json`, SHA-256 integrity). Restore maps
`guest.ram.lazy` with `MAP_PRIVATE` so pages fault in on demand. Create and
restore times show up in `ternvale stats` (`snapshot create N ms` /
`snapshot restore N ms`). Host-side device writes into guest RAM mark dirty
pages explicitly (`GuestMemory::write_bytes` / `host_ptr_mut`); guest stores
are tracked with stage-2 `hv_vm_protect`.
