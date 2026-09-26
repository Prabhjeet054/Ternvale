//! Virtio-mmio v2 (virtio 1.x) transport.
//!
//! Register layout follows the virtio 1.2 MMIO chapter. Feature negotiation
//! accepts `FEATURES_OK` only when the driver offers `VIRTIO_F_VERSION_1`.

mod blk;
mod irq;
mod mmio;
mod queue;

pub use blk::{
    AttachedBlk, BlkStats, VirtioBlk, VirtioBlkError, VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_RO,
    VIRTIO_BLK_ID,
};
pub use irq::{IrqHook, VirtioIrq};
pub use mmio::{slot_base, VirtioMmio, VirtioMmioError};
pub use queue::{Buffer, Chain, SplitQueue};

/// `VIRTIO_F_VERSION_1`. Required before `FEATURES_OK`.
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;
/// Indirect descriptor tables. Bit 28.
pub const VIRTIO_F_INDIRECT_DESC: u64 = 1 << 28;
/// Used and available event indices. Bit 29.
pub const VIRTIO_F_EVENT_IDX: u64 = 1 << 29;

/// Status bit set by the driver after it finds the device.
pub const STATUS_ACKNOWLEDGE: u32 = 1;
/// Status bit set once a driver is bound.
pub const STATUS_DRIVER: u32 = 2;
/// Status bit set when the driver finishes initialization.
pub const STATUS_DRIVER_OK: u32 = 4;
/// Status bit set after feature negotiation.
pub const STATUS_FEATURES_OK: u32 = 8;
/// Device asks the driver to reset.
pub const STATUS_DEVICE_NEEDS_RESET: u32 = 64;
/// Driver gave up on the device.
pub const STATUS_FAILED: u32 = 128;

/// A ready virtqueue the driver just kicked.
#[derive(Debug, Clone)]
pub struct QueueNotify {
    /// Queue index from `QueueNotify`.
    pub index: u16,
    /// Negotiated queue size.
    pub size: u16,
    /// Guest descriptor table address.
    pub desc: u64,
    /// Guest available ring address (`QueueDriver`).
    pub avail: u64,
    /// Guest used ring address (`QueueDevice`).
    pub used: u64,
    /// Features the driver accepted.
    pub features: u64,
}

/// A virtio 1.x device behind the MMIO transport.
pub trait VirtioDevice: Send {
    /// Virtio device id (`VIRTIO_ID_*`). Zero means an empty slot.
    fn device_id(&self) -> u32;

    /// Four-byte vendor id. The QEMU value is `0x554d4551`.
    fn vendor_id(&self) -> u32 {
        0x554d_4551
    }

    /// Device feature bits, not including [`VIRTIO_F_VERSION_1`].
    fn device_features(&self) -> u64 {
        0
    }

    /// How many virtqueues this device has.
    fn num_queues(&self) -> u16 {
        1
    }

    /// Maximum queue size for `queue`.
    fn queue_num_max(&self, _queue: u16) -> u16 {
        256
    }

    /// Read device-specific config at `offset` from the start of config space.
    fn read_config(&mut self, _offset: u64, _size: u8) -> u64 {
        0
    }

    /// Write device-specific config.
    fn write_config(&mut self, _offset: u64, _size: u8, _value: u64) {}

    /// The driver kicked a ready queue.
    fn notify(&mut self, _queue: QueueNotify) {}

    /// `status` changed, including a reset to 0.
    fn status_changed(&mut self, _status: u32) {}

    /// Bumped when config space changes.
    fn config_generation(&self) -> u32 {
        0
    }
}

/// A virtio device that only answers identity and queue-size registers.
pub struct FixedDevice {
    id: u32,
    queues: u16,
}

impl FixedDevice {
    /// `id` is the virtio device id. `queues` is how many virtqueues it reports.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::mmio", fields(id, queues))]
    pub fn new(id: u32, queues: u16) -> Self {
        Self { id, queues }
    }
}

impl VirtioDevice for FixedDevice {
    fn device_id(&self) -> u32 {
        self.id
    }

    fn num_queues(&self) -> u16 {
        self.queues
    }
}

#[cfg(test)]
#[path = "virtio_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "virtio_hv_test.rs"]
mod hv_test;

#[cfg(test)]
#[path = "blk_hv_test.rs"]
mod blk_hv_test;
