//! Shared metric tables. Registration allocates; recording does not.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use crate::counter::Counter;
use crate::histogram::Histogram;
use crate::kind::{ExitKind, EXIT_KINDS};
use crate::snapshot::{DeviceSnapshot, IoSnapshot, Snapshot, VcpuSnapshot};

/// Guest CPU count the config allows.
pub const MAX_VCPUS: usize = 16;
/// MMIO devices that can carry their own latency histogram.
pub const MAX_DEVICES: usize = 64;
/// Named virtio-blk disks with their own latency histogram.
pub const MAX_BLK_DISKS: usize = 16;

/// Lock-free counters and histograms for one VM.
#[derive(Debug)]
pub struct Metrics {
    vcpus: [VcpuCell; MAX_VCPUS],
    devices: [DeviceCell; MAX_DEVICES],
    blk: IoCell,
    disks: [BlkDiskCell; MAX_BLK_DISKS],
    net: IoCell,
    interrupts: Counter,
}

#[derive(Debug)]
struct VcpuCell {
    exits: [AtomicU64; EXIT_KINDS],
    guest_ns: AtomicU64,
    handle_ns: AtomicU64,
}

#[derive(Debug)]
struct DeviceCell {
    name: OnceLock<String>,
    accesses: AtomicU64,
    latency: Histogram,
}

#[derive(Debug, Default)]
struct IoCell {
    count: Counter,
    bytes: Counter,
    latency: Histogram,
}

#[derive(Debug, Default)]
struct BlkDiskCell {
    name: OnceLock<String>,
    io: IoCell,
}

impl Default for VcpuCell {
    fn default() -> Self {
        Self {
            exits: std::array::from_fn(|_| AtomicU64::new(0)),
            guest_ns: AtomicU64::new(0),
            handle_ns: AtomicU64::new(0),
        }
    }
}

impl Default for DeviceCell {
    fn default() -> Self {
        Self {
            name: OnceLock::new(),
            accesses: AtomicU64::new(0),
            latency: Histogram::default(),
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// Empty tables for one VM.
    #[tracing::instrument(level = "debug", target = "ternvale::metrics", skip_all)]
    pub fn new() -> Self {
        tracing::debug!(target: "ternvale::metrics", "metrics tables created");
        Self {
            vcpus: std::array::from_fn(|_| VcpuCell::default()),
            devices: std::array::from_fn(|_| DeviceCell::default()),
            blk: IoCell::default(),
            disks: std::array::from_fn(|_| BlkDiskCell::default()),
            net: IoCell::default(),
            interrupts: Counter::default(),
        }
    }

    /// Bind `name` to a device slot. Not a hot path.
    #[tracing::instrument(level = "debug", target = "ternvale::metrics", skip_all, fields(device = name))]
    pub fn register_device(&self, name: &str) -> Option<u8> {
        for (index, cell) in self.devices.iter().enumerate() {
            if cell.name.get().is_some_and(|have| have == name) {
                return u8::try_from(index).ok();
            }
        }
        for (index, cell) in self.devices.iter().enumerate() {
            if cell.name.set(name.to_string()).is_ok() {
                tracing::debug!(
                    target: "ternvale::metrics",
                    device = name,
                    id = index,
                    "registered metrics device"
                );
                return u8::try_from(index).ok();
            }
            if cell.name.get().is_some_and(|have| have == name) {
                return u8::try_from(index).ok();
            }
        }
        tracing::warn!(
            target: "ternvale::metrics",
            device = name,
            max = MAX_DEVICES,
            "metrics device table full"
        );
        None
    }

    /// Bind `name` to a virtio-blk disk histogram. Not a hot path.
    #[tracing::instrument(level = "debug", target = "ternvale::metrics", skip_all, fields(disk = name))]
    pub fn register_blk_disk(&self, name: &str) -> Option<u8> {
        for (index, cell) in self.disks.iter().enumerate() {
            if cell.name.get().is_some_and(|have| have == name) {
                return u8::try_from(index).ok();
            }
        }
        for (index, cell) in self.disks.iter().enumerate() {
            if cell.name.set(name.to_string()).is_ok() {
                tracing::debug!(
                    target: "ternvale::metrics",
                    disk = name,
                    id = index,
                    "registered blk disk metrics"
                );
                return u8::try_from(index).ok();
            }
            if cell.name.get().is_some_and(|have| have == name) {
                return u8::try_from(index).ok();
            }
        }
        tracing::warn!(
            target: "ternvale::metrics",
            disk = name,
            max = MAX_BLK_DISKS,
            "metrics blk disk table full"
        );
        None
    }

    /// Count one exit of `kind` on `cpu`. Hot path: atomics only.
    #[inline]
    pub fn record_exit(&self, cpu: u32, kind: ExitKind) {
        let Some(cell) = self.vcpus.get(cpu as usize) else {
            return;
        };
        cell.exits[kind.index()].fetch_add(1, Ordering::Relaxed);
    }

    /// Add guest (`hv_vcpu_run`) time. Hot path.
    #[inline]
    pub fn record_guest(&self, cpu: u32, elapsed: Duration) {
        if let Some(cell) = self.vcpus.get(cpu as usize) {
            cell.guest_ns.fetch_add(nanos(elapsed), Ordering::Relaxed);
        }
    }

    /// Add exit-handling time. Hot path.
    #[inline]
    pub fn record_handle(&self, cpu: u32, elapsed: Duration) {
        if let Some(cell) = self.vcpus.get(cpu as usize) {
            cell.handle_ns.fetch_add(nanos(elapsed), Ordering::Relaxed);
        }
    }

    /// One MMIO access and its device-call duration. Hot path.
    #[inline]
    pub fn record_mmio(&self, id: u8, elapsed: Duration) {
        let Some(cell) = self.devices.get(id as usize) else {
            return;
        };
        cell.accesses.fetch_add(1, Ordering::Relaxed);
        cell.latency.record(nanos(elapsed));
    }

    /// Completed virtio-blk requests from one virtqueue notify (VM total).
    #[inline]
    pub fn record_blk(&self, requests: u64, bytes: u64, complete: Option<Duration>) {
        record_io(&self.blk, requests, bytes, complete);
    }

    /// Completed requests on one registered disk, also counted in [`Self::record_blk`].
    #[inline]
    pub fn record_blk_disk(&self, id: u8, requests: u64, bytes: u64, complete: Option<Duration>) {
        record_io(&self.blk, requests, bytes, complete);
        if let Some(cell) = self.disks.get(id as usize) {
            record_io(&cell.io, requests, bytes, complete);
        }
    }

    /// virtio-net notify-to-completion (one kick), plus payload bytes.
    #[inline]
    pub fn record_net(&self, packets: u64, bytes: u64, complete: Option<Duration>) {
        self.net.count.add(packets);
        self.net.bytes.add(bytes);
        if let Some(elapsed) = complete {
            self.net.latency.record(nanos(elapsed));
        }
    }

    /// One interrupt injection that asserted a GIC line.
    #[inline]
    pub fn record_interrupt(&self) {
        self.interrupts.inc();
    }

    /// Copy every counter for `query-stats`.
    #[tracing::instrument(level = "debug", target = "ternvale::metrics", skip_all)]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            vcpus: self.vcpu_snapshots(),
            devices: self.device_snapshots(),
            blk: io_snapshot(&self.blk),
            disks: self.disk_snapshots(),
            net: io_snapshot(&self.net),
            interrupts: self.interrupts.get(),
        }
    }

    /// INFO summary on VM shutdown.
    #[tracing::instrument(level = "debug", target = "ternvale::metrics", skip_all, fields(vm))]
    pub fn log_summary(&self, vm: &str) {
        let snap = self.snapshot();
        let exits: u64 = snap
            .vcpus
            .iter()
            .flat_map(|cpu| cpu.exits.iter().map(|e| e.count))
            .sum();
        tracing::info!(
            target: "ternvale::metrics",
            vm,
            exits,
            interrupts = snap.interrupts,
            blk_reqs = snap.blk.count,
            blk_bytes = snap.blk.bytes,
            blk_p50_ns = snap.blk.latency.p50_ns,
            blk_p99_ns = snap.blk.latency.p99_ns,
            blk_disks = snap.disks.len(),
            net_count = snap.net.count,
            net_bytes = snap.net.bytes,
            net_p50_ns = snap.net.latency.p50_ns,
            "vm metrics summary"
        );
    }

    fn vcpu_snapshots(&self) -> Vec<VcpuSnapshot> {
        self.vcpus
            .iter()
            .enumerate()
            .filter_map(|(cpu, cell)| {
                let guest_ns = cell.guest_ns.load(Ordering::Relaxed);
                let handle_ns = cell.handle_ns.load(Ordering::Relaxed);
                let exits: Vec<_> = ExitKind::ALL
                    .iter()
                    .map(|kind| {
                        crate::snapshot::ExitCount::new(
                            *kind,
                            cell.exits[kind.index()].load(Ordering::Relaxed),
                        )
                    })
                    .collect();
                let total: u64 = exits.iter().map(|e| e.count).sum();
                if guest_ns == 0 && handle_ns == 0 && total == 0 {
                    return None;
                }
                Some(VcpuSnapshot {
                    cpu: cpu as u32,
                    guest_ns,
                    handle_ns,
                    exits,
                })
            })
            .collect()
    }

    fn device_snapshots(&self) -> Vec<DeviceSnapshot> {
        self.devices
            .iter()
            .filter_map(|cell| {
                let name = cell.name.get()?;
                Some(DeviceSnapshot {
                    name: name.clone(),
                    accesses: cell.accesses.load(Ordering::Relaxed),
                    latency: cell.latency.snapshot(),
                })
            })
            .collect()
    }

    fn disk_snapshots(&self) -> Vec<crate::snapshot::NamedIoSnapshot> {
        self.disks
            .iter()
            .filter_map(|cell| {
                let name = cell.name.get()?;
                Some(crate::snapshot::NamedIoSnapshot {
                    name: name.clone(),
                    io: io_snapshot(&cell.io),
                })
            })
            .collect()
    }
}

fn record_io(cell: &IoCell, requests: u64, bytes: u64, complete: Option<Duration>) {
    cell.count.add(requests);
    cell.bytes.add(bytes);
    if let Some(elapsed) = complete {
        cell.latency.record(nanos(elapsed));
    }
}

fn io_snapshot(cell: &IoCell) -> IoSnapshot {
    IoSnapshot {
        count: cell.count.get(),
        bytes: cell.bytes.get(),
        latency: cell.latency.snapshot(),
    }
}

#[inline]
fn nanos(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::Metrics;
    use crate::kind::ExitKind;
    use std::time::Duration;

    #[test]
    fn snapshot_keeps_vcpu_and_device_and_io() {
        let metrics = Metrics::new();
        metrics.record_exit(0, ExitKind::Mmio);
        metrics.record_guest(0, Duration::from_nanos(1_000));
        metrics.record_handle(0, Duration::from_nanos(50));
        let id = metrics.register_device("uart").expect("id");
        metrics.record_mmio(id, Duration::from_nanos(25));
        metrics.record_blk(1, 512, Some(Duration::from_micros(10)));
        let disk = metrics.register_blk_disk("root.img").expect("disk");
        metrics.record_blk_disk(disk, 2, 1024, Some(Duration::from_micros(20)));
        metrics.record_net(1, 64, Some(Duration::from_micros(2)));
        metrics.record_interrupt();
        let snap = metrics.snapshot();
        assert_eq!(snap.vcpus[0].exits[ExitKind::Mmio.index()].count, 1);
        assert_eq!(snap.devices[0].name, "uart");
        assert_eq!(snap.devices[0].accesses, 1);
        assert_eq!(snap.blk.count, 3);
        assert_eq!(snap.blk.bytes, 1536);
        assert_eq!(snap.disks.len(), 1);
        assert_eq!(snap.disks[0].name, "root.img");
        assert_eq!(snap.disks[0].io.count, 2);
        assert_eq!(snap.net.count, 1);
        assert_eq!(snap.interrupts, 1);
    }
}
