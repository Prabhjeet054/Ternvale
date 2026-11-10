# Ternvale

Ternvale is a virtual machine monitor for **Apple Silicon** Macs. Guests are
**ARM64 only**. There is no x86 (or Rosetta) emulation.

You can run it from the **Ternvale** Mac app or the `ternvale` CLI (the
helper inside `Ternvale.app/Contents/Helpers/`). Both talk to the same VMM.
The app does not duplicate VM lifecycle: it uses the control socket.

Requirements: Apple Silicon, macOS 14+, Hypervisor.framework (the binary must
be codesigned with `com.apple.security.hypervisor`).

This book is the user guide. Privacy, terms, Windows licensing, and the
QEMU+HVF performance notes live in `docs/` of the source tree
(`PRIVACY.md`, `TERMS.md`, `WINDOWS_LICENSE.md`, `PERF_REPORT.md`).
Third-party crates and optional guest tools: `THIRD_PARTY_NOTICES.md`.
