//! Venus-oriented virtio-gpu 3D path (feature `gpu3d`).
//!
//! Per-VM `[gpu] gpu3d = false` keeps this path off even in a `gpu-3d` build.
//! Host rendering lives in `ternvale-gpu3d-host` (virglrenderer → MoltenVK).

mod backend;
mod capset;

pub use backend::Gpu3d;
#[cfg(test)]
pub use capset::VENUS_CAPSET_BYTES;
pub use capset::{CAPSET_COUNT, CAPSET_VENUS};
