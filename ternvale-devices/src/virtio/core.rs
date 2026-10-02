//! Transport-independent virtio state: features, status, queues, interrupt.
//!
//! virtio-mmio and virtio-pci own their register layouts and select registers.
//! This core owns the state behind them, so the status machine, feature checks,
//! queue validation, and reset behave the same on both. Logs go to
//! `ternvale::virtio::transport` and carry the transport `name`
//! (`virtio-mmio-<slot>` or `virtio-pci-<bdf>`).

mod queue;
mod status;

use std::sync::Arc;

use super::irq::VirtioIrq;
use super::{VirtioDevice, VIRTIO_F_VERSION_1};

/// One virtqueue as the driver configured it.
#[derive(Debug)]
pub(crate) struct Queue {
    pub(crate) num_max: u32,
    pub(crate) num: u32,
    pub(crate) ready: bool,
    pub(crate) desc: u64,
    pub(crate) driver: u64,
    pub(crate) device: u64,
}

/// Which ring address a transport register writes.
#[derive(Debug, Clone, Copy)]
pub(crate) enum QueueAddr {
    Desc,
    Driver,
    Device,
}

/// Device, negotiated features, status, and queues shared by both transports.
pub(crate) struct VirtioCore {
    pub(crate) device: Box<dyn VirtioDevice>,
    pub(crate) name: String,
    pub(crate) driver_features: u64,
    pub(crate) queues: Vec<Queue>,
    pub(crate) status: u32,
    pub(crate) interrupt: Arc<VirtioIrq>,
}

impl VirtioCore {
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::transport",
        skip_all,
        fields(name = %name, device_id = device.device_id())
    )]
    pub(crate) fn new(
        name: String,
        device: Box<dyn VirtioDevice>,
        interrupt: Arc<VirtioIrq>,
    ) -> Self {
        let queues = (0..device.num_queues())
            .map(|index| Queue {
                num_max: u32::from(device.queue_num_max(index)),
                num: 0,
                ready: false,
                desc: 0,
                driver: 0,
                device: 0,
            })
            .collect();
        Self {
            device,
            name,
            driver_features: 0,
            queues,
            status: 0,
            interrupt,
        }
    }

    /// Device features plus `VIRTIO_F_VERSION_1`, which every transport offers.
    pub(crate) fn offered(&self) -> u64 {
        self.device.device_features() | VIRTIO_F_VERSION_1
    }

    pub(crate) fn queue(&self, index: u32) -> Option<&Queue> {
        self.queues.get(index as usize)
    }

    pub(crate) fn queue_mut(&mut self, index: u32) -> Option<&mut Queue> {
        self.queues.get_mut(index as usize)
    }

    pub(crate) fn warn_queue(&self, queue: u32, message: &'static str) {
        tracing::warn!(
            target: "ternvale::virtio::transport",
            name = %self.name,
            queue,
            message
        );
    }
}

/// 32-bit word `sel` of a 64-bit feature set. Words past 1 read as 0.
pub(crate) fn feature_word(features: u64, sel: u32) -> u32 {
    match sel {
        0 => features as u32,
        1 => (features >> 32) as u32,
        _ => 0,
    }
}

/// Keep the low `size` bytes of `value`.
pub(crate) fn mask(value: u64, size: u8) -> u64 {
    match size {
        1 => value & 0xff,
        2 => value & 0xffff,
        4 => value & 0xffff_ffff,
        _ => value,
    }
}
