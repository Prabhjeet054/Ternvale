//! Serializable copies of [`crate::Metrics`].

use serde::{Deserialize, Serialize};

use crate::histogram::HistSnapshot;
use crate::kind::ExitKind;

/// Full metrics snapshot for `query-stats`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Per-vCPU exits and run/handle time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vcpus: Vec<VcpuSnapshot>,
    /// Per-MMIO-device access counts and handling latency.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<DeviceSnapshot>,
    /// virtio-blk requests and notify-to-completion latency (all disks).
    pub blk: IoSnapshot,
    /// Per-disk virtio-blk latency. Empty on VMs with no named disks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<NamedIoSnapshot>,
    /// virtio-net packets and notify-to-completion latency.
    pub net: IoSnapshot,
    /// `hv_gic_set_spi` / PPI injections that asserted a line.
    pub interrupts: u64,
    /// Guest-TX policy drops by rule name. Empty when none have fired.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub net_policy: Vec<NamedCount>,
}

/// One vCPU in [`Snapshot::vcpus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VcpuSnapshot {
    /// Guest CPU index.
    pub cpu: u32,
    /// Wall nanoseconds inside `hv_vcpu_run`.
    pub guest_ns: u64,
    /// Wall nanoseconds handling exits after `hv_vcpu_run` returned.
    pub handle_ns: u64,
    /// Exit counts by [`ExitKind::as_str`].
    pub exits: Vec<ExitCount>,
}

/// One exit class total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitCount {
    /// [`ExitKind::as_str`].
    pub kind: String,
    /// How many of this kind.
    pub count: u64,
}

impl ExitCount {
    /// Pair a kind with its counter.
    #[tracing::instrument(level = "debug", target = "ternvale::metrics", skip_all)]
    pub fn new(kind: ExitKind, count: u64) -> Self {
        Self {
            kind: kind.as_str().to_string(),
            count,
        }
    }
}

/// One MMIO window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSnapshot {
    /// Device name from the MMIO bus.
    pub name: String,
    /// Accesses dispatched to it.
    pub accesses: u64,
    /// Time spent in the device `read`/`write` call.
    pub latency: HistSnapshot,
}

/// One named counter (policy rule, and similar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedCount {
    /// Rule or series name (`spoof_mac`, `allow_lan`, …).
    pub name: String,
    /// How many times it fired.
    pub count: u64,
}

/// One named virtio-blk device in [`Snapshot::disks`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedIoSnapshot {
    /// Image file name (or `"disk"`).
    pub name: String,
    /// Request counts and latency for this disk.
    #[serde(flatten)]
    pub io: IoSnapshot,
}

/// Request or packet I/O plus completion latency.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoSnapshot {
    /// Completed requests (blk) or packets (net).
    pub count: u64,
    /// Payload bytes.
    pub bytes: u64,
    /// Notify-to-completion latency.
    pub latency: HistSnapshot,
}
