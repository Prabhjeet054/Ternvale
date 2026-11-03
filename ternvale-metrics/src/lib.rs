//! Lock-free VM counters and log-scale latency histograms.
//!
//! Hot-path record methods only `fetch_add` / CAS. Snapshots and device
//! registration may allocate.

mod counter;
mod histogram;
mod kind;
mod registry;
mod snapshot;

pub use counter::Counter;
pub use histogram::{HistSnapshot, Histogram, BUCKETS};
pub use kind::{ExitKind, EXIT_KINDS};
pub use registry::{Metrics, MAX_BLK_DISKS, MAX_DEVICES, MAX_VCPUS};
pub use snapshot::{
    DeviceSnapshot, ExitCount, IoSnapshot, NamedIoSnapshot, Snapshot, VcpuSnapshot,
};
