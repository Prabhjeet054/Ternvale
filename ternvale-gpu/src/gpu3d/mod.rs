//! Venus-oriented virtio-gpu 3D path (feature `gpu3d`, off by default).
//!
//! Upstream [`rutabaga_gfx`](https://chromium.googlesource.com/chromiumos/platform/crosvm/+/refs/heads/main/rutabaga_gfx/)
//! from crosvm is **BSD-3-Clause** (Chromium OS Authors) and is license-compatible with
//! Ternvale reuse (see `docs/GPU_ACCELERATION.md`). It is not linked in-tree yet because it
//! pulls `virglrenderer` through pkg-config; this module implements the same virtio-gpu 3D
//! command surface so a future build can delegate to `Rutabaga::handle_*` without changing
//! the guest contract.

mod backend;
mod capset;
mod virgl;

pub use backend::Gpu3d;
#[cfg(test)]
pub use capset::VENUS_CAPSET_BYTES;
pub use capset::{CAPSET_COUNT, CAPSET_VENUS};
