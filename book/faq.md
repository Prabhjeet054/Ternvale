# FAQ

**Does Ternvale run x86 Windows or Linux?**  
No. ARM64 guests only.

**Can I run macOS in a VM?**  
Not with Hypervisor.framework alone. Apple’s supported path is
Virtualization.framework.

**Where is the CLI after I install the app?**  
`/Applications/Ternvale.app/Contents/Helpers/ternvale` (not on `PATH`
unless you add it).

**Do I need a Windows licence?**  
Yes, if you run Windows. Ternvale does not include one. See
`docs/WINDOWS_LICENSE.md` in the source tree.

**Is there telemetry?**  
No, unless you opt in to crash reports and confirm the preview.
See `docs/PRIVACY.md`.

**Where is my data?**  
VMs: `~/Library/Application Support/Ternvale/`. Logs:
`~/Library/Logs/Ternvale/`.

**How do I update?**  
**Ternvale → Check for Updates…** or Settings (stable/beta). Automatic
install stays off until you enable **Download and install updates
automatically**.

**How do I take a snapshot or share a folder?**  
CLI / `config.toml` only in this release. Pause before snapshot restore.
One virtio-fs share, and `[vsock] agent = true`.

**GPL code?**  
The Rust graph has none (`cargo deny check licenses`). WinFsp is GPL and is
**not** linked; guests may install the unmodified MSI. Details in
`THIRD_PARTY_NOTICES.md`.

**Why no numbers vs other Mac hypervisors?**  
`docs/PERF_REPORT.md` compares Ternvale only to **QEMU + HVF** from the
in-tree harness. Some commercial products forbid publishing benchmarks;
do not add those tables until you have read their licence terms.
