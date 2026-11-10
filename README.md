# Ternvale

Ternvale is a virtual machine monitor for Apple Silicon Macs. It runs ARM64 guests on
`aarch64-apple-darwin` using Hypervisor.framework. There is no x86 emulation.

The name comes from a tern (a bird that carries itself between places) and a vale (a sheltered
valley for the guest to run in).

This repository is a Cargo workspace. The crates below are stubs: they compile, and they do not
implement the VMM yet.

## Crates

| Crate | Role |
| --- | --- |
| `ternvale-cli` | Command-line interface |
| `ternvale-vmm` | VM lifecycle, vCPU, and memory orchestration |
| `ternvale-hv` | Safe wrappers around Hypervisor.framework |
| `ternvale-devices` | Emulated and virtio devices |
| `ternvale-config` | VM configuration and shared errors |
| `ternvale-log` | Tracing setup, panic hook, and log files |
| `ternvale-sandbox` | Seatbelt profile, security-scoped bookmarks, helper codesign |

`ternvale-config` and `ternvale-log` are used across the stack. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build

Requirements: Apple Silicon Mac, Rust stable via rustup (the pinned toolchain is in
`rust-toolchain.toml`).

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Guest-facing parsers have libFuzzer targets in `ternvale-fuzz` (nightly +
`cargo-fuzz`). `make fuzz-smoke` runs each target for 30 seconds;
`make fuzz-long` is the long campaign (see [docs/FUZZING.md](docs/FUZZING.md)).

## Docs

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — layering
- [docs/DEBUGGING.md](docs/DEBUGGING.md) — triage headings (filled in as the VMM grows)
- [docs/PROGRESS.md](docs/PROGRESS.md) — what each step changed and how to test it
- [docs/IMPORT.md](docs/IMPORT.md) — `.tvm` export/import, templates, qcow2/vmdk/vhdx
- [docs/CI.md](docs/CI.md) — GitHub Actions, hosted vs self-hosted HV, runner security
- Docs-only CI path (Step 87 test): README and `docs/*.md` changes skip fmt/clippy/test/fuzz.
