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

## Docs

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — layering
- [docs/DEBUGGING.md](docs/DEBUGGING.md) — triage headings (filled in as the VMM grows)
- [docs/PROGRESS.md](docs/PROGRESS.md) — what each step changed and how to test it
