//! Virtio-mmio v2 register file.
//!
//! Status, feature, and queue state lives in [`VirtioCore`]; this file maps
//! the virtio-mmio register offsets onto it and owns the select registers.

use std::sync::Arc;

use ternvale_vmm::{
    MmioBus, MmioDevice, MmioError, VIRTIO_MMIO_BASE, VIRTIO_MMIO_SLOTS, VIRTIO_MMIO_SLOT_SIZE,
};

use super::core::{feature_word, mask, QueueAddr, VirtioCore};
use super::irq::VirtioIrq;
use super::VirtioDevice;

const MAGIC: u32 = 0x7472_6976;
const VERSION: u32 = 2;
const MAGIC_VALUE: u64 = 0x000;
const VERSION_REG: u64 = 0x004;
const DEVICE_ID: u64 = 0x008;
const VENDOR_ID: u64 = 0x00c;
const DEVICE_FEATURES: u64 = 0x010;
const DEVICE_FEATURES_SEL: u64 = 0x014;
const DRIVER_FEATURES: u64 = 0x020;
const DRIVER_FEATURES_SEL: u64 = 0x024;
const QUEUE_SEL: u64 = 0x030;
const QUEUE_NUM_MAX: u64 = 0x034;
const QUEUE_NUM: u64 = 0x038;
const QUEUE_READY: u64 = 0x044;
const QUEUE_NOTIFY: u64 = 0x050;
const INTERRUPT_STATUS: u64 = 0x060;
const INTERRUPT_ACK: u64 = 0x064;
const STATUS: u64 = 0x070;
const QUEUE_DESC_LOW: u64 = 0x080;
const QUEUE_DESC_HIGH: u64 = 0x084;
const QUEUE_DRIVER_LOW: u64 = 0x090;
const QUEUE_DRIVER_HIGH: u64 = 0x094;
const QUEUE_DEVICE_LOW: u64 = 0x0a0;
const QUEUE_DEVICE_HIGH: u64 = 0x0a4;
const CONFIG_GENERATION: u64 = 0x0fc;
const CONFIG: u64 = 0x100;

/// Guest PA of virtio-mmio `slot`.
#[tracing::instrument(level = "debug", target = "ternvale::virtio::mmio", fields(slot))]
pub fn slot_base(slot: u32) -> Option<u64> {
    if u64::from(slot) >= VIRTIO_MMIO_SLOTS {
        tracing::warn!(
            target: "ternvale::virtio::mmio",
            slot,
            slots = VIRTIO_MMIO_SLOTS,
            "virtio-mmio slot is outside the platform window"
        );
        return None;
    }
    Some(VIRTIO_MMIO_BASE + u64::from(slot) * VIRTIO_MMIO_SLOT_SIZE)
}

/// Registering a virtio-mmio slot failed.
#[derive(Debug, thiserror::Error)]
pub enum VirtioMmioError {
    /// `slot` is not one of the platform virtio-mmio windows.
    #[error("virtio-mmio slot {slot} is outside 0..{slots}")]
    Slot {
        /// Requested slot.
        slot: u32,
        /// Slot count from the platform layout.
        slots: u32,
    },
    /// The MMIO bus rejected the window.
    #[error("register virtio-mmio slot {slot} at {base:#x}: {source}")]
    Bus {
        /// Requested slot.
        slot: u32,
        /// Guest address of the slot.
        base: u64,
        /// Bus error.
        source: MmioError,
    },
}

/// Virtio-mmio v2 transport for one [`VirtioDevice`].
pub struct VirtioMmio {
    core: VirtioCore,
    device_features_sel: u32,
    driver_features_sel: u32,
    queue_sel: u32,
}

impl VirtioMmio {
    /// Build a transport. `VIRTIO_F_VERSION_1` is always offered.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::mmio",
        skip_all,
        fields(slot, device_id = device.device_id())
    )]
    pub fn new(slot: u32, device: Box<dyn VirtioDevice>) -> Self {
        Self::with_irq(slot, device, VirtioIrq::new())
    }

    /// Build a transport that shares `interrupt` with the device worker.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::mmio",
        skip_all,
        fields(slot, device_id = device.device_id())
    )]
    pub fn with_irq(slot: u32, device: Box<dyn VirtioDevice>, interrupt: Arc<VirtioIrq>) -> Self {
        tracing::info!(
            target: "ternvale::virtio::mmio",
            slot,
            device_id = device.device_id(),
            queues = device.num_queues(),
            "virtio-mmio transport created"
        );
        Self {
            core: VirtioCore::new(format!("virtio-mmio-{slot}"), device, interrupt),
            device_features_sel: 0,
            driver_features_sel: 0,
            queue_sel: 0,
        }
    }

    /// Shared interrupt line for this slot.
    pub fn irq(&self) -> Arc<VirtioIrq> {
        Arc::clone(&self.core.interrupt)
    }

    /// Map this transport onto platform virtio-mmio `slot`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::mmio",
        skip_all,
        fields(slot, device_id = device.device_id())
    )]
    pub fn register(
        bus: &mut MmioBus,
        slot: u32,
        device: Box<dyn VirtioDevice>,
    ) -> Result<(), VirtioMmioError> {
        let Some(base) = slot_base(slot) else {
            return Err(VirtioMmioError::Slot {
                slot,
                slots: VIRTIO_MMIO_SLOTS as u32,
            });
        };
        let transport = Self::new(slot, device);
        bus.register(base, VIRTIO_MMIO_SLOT_SIZE, Box::new(transport))
            .map_err(|source| VirtioMmioError::Bus { slot, base, source })?;
        tracing::info!(
            target: "ternvale::virtio::mmio",
            slot,
            base = format!("{base:#x}"),
            size = format!("{:#x}", VIRTIO_MMIO_SLOT_SIZE),
            "virtio-mmio registered"
        );
        Ok(())
    }

    /// Raise interrupt-status bits. The driver clears them through `InterruptACK`.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::mmio", skip(self), fields(name = %self.core.name, bits))]
    pub fn raise_interrupt(&mut self, bits: u32) {
        self.core.interrupt.raise(bits);
    }

    /// Guest read of one register or a config-space field.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::mmio",
        skip(self),
        fields(name = %self.core.name, offset = format!("{offset:#x}"), size)
    )]
    pub fn read(&mut self, offset: u64, size: u8) -> u64 {
        let value = if offset >= CONFIG {
            self.core.device.read_config(offset - CONFIG, size)
        } else if size != 4 {
            self.warn_size(offset, size);
            0
        } else {
            self.read_reg(offset)
        };
        self.trace(offset, size, value, "read");
        mask(value, size)
    }

    /// Guest write of one register or a config-space field.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::mmio",
        skip(self),
        fields(name = %self.core.name, offset = format!("{offset:#x}"), size, value = format!("{value:#x}"))
    )]
    pub fn write(&mut self, offset: u64, size: u8, value: u64) {
        self.trace(offset, size, value, "write");
        if offset >= CONFIG {
            self.core.device.write_config(offset - CONFIG, size, value);
            return;
        }
        if size != 4 {
            self.warn_size(offset, size);
            return;
        }
        self.write_reg(offset, value as u32);
    }

    fn read_reg(&mut self, offset: u64) -> u64 {
        let core = &self.core;
        let value = match offset {
            MAGIC_VALUE => MAGIC,
            VERSION_REG => VERSION,
            DEVICE_ID => core.device.device_id(),
            VENDOR_ID => core.device.vendor_id(),
            DEVICE_FEATURES => feature_word(core.offered(), self.device_features_sel),
            QUEUE_NUM_MAX => core.queue(self.queue_sel).map_or(0, |q| q.num_max),
            QUEUE_READY => u32::from(core.queue(self.queue_sel).is_some_and(|q| q.ready)),
            INTERRUPT_STATUS => core.interrupt.status(),
            STATUS => core.status,
            CONFIG_GENERATION => core.device.config_generation(),
            _ => {
                tracing::warn!(
                    target: "ternvale::virtio::mmio",
                    name = %core.name,
                    offset = format!("{offset:#x}"),
                    "read of write-only or unknown virtio-mmio register"
                );
                0
            }
        };
        u64::from(value)
    }

    fn write_reg(&mut self, offset: u64, value: u32) {
        let sel = self.queue_sel;
        match offset {
            DEVICE_FEATURES_SEL => self.device_features_sel = value,
            DRIVER_FEATURES_SEL => self.driver_features_sel = value,
            DRIVER_FEATURES => self
                .core
                .write_driver_features(self.driver_features_sel, value),
            QUEUE_SEL => self.queue_sel = value,
            QUEUE_NUM => self.core.set_queue_num(sel, value),
            QUEUE_READY => self.core.set_queue_ready(sel, value),
            QUEUE_NOTIFY => self.core.notify(value),
            INTERRUPT_ACK => self.core.interrupt.ack(value),
            STATUS => {
                if value == 0 {
                    self.device_features_sel = 0;
                    self.driver_features_sel = 0;
                    self.queue_sel = 0;
                }
                self.core.write_status(value);
            }
            QUEUE_DESC_LOW => self.core.set_queue_addr(sel, QueueAddr::Desc, false, value),
            QUEUE_DESC_HIGH => self.core.set_queue_addr(sel, QueueAddr::Desc, true, value),
            QUEUE_DRIVER_LOW => self
                .core
                .set_queue_addr(sel, QueueAddr::Driver, false, value),
            QUEUE_DRIVER_HIGH => self
                .core
                .set_queue_addr(sel, QueueAddr::Driver, true, value),
            QUEUE_DEVICE_LOW => self
                .core
                .set_queue_addr(sel, QueueAddr::Device, false, value),
            QUEUE_DEVICE_HIGH => self
                .core
                .set_queue_addr(sel, QueueAddr::Device, true, value),
            _ => tracing::warn!(
                target: "ternvale::virtio::mmio",
                name = %self.core.name,
                offset = format!("{offset:#x}"),
                value = format!("{value:#x}"),
                "write to read-only or unknown virtio-mmio register"
            ),
        }
    }

    fn warn_size(&self, offset: u64, size: u8) {
        tracing::warn!(
            target: "ternvale::virtio::mmio",
            name = %self.core.name,
            offset = format!("{offset:#x}"),
            size,
            "virtio-mmio register access is not 4 bytes"
        );
    }

    fn trace(&self, offset: u64, size: u8, value: u64, direction: &'static str) {
        tracing::trace!(
            target: "ternvale::virtio::mmio",
            name = %self.core.name,
            offset = format!("{offset:#x}"),
            size,
            value = format!("{value:#x}"),
            direction,
            "virtio-mmio register"
        );
    }
}

impl MmioDevice for VirtioMmio {
    fn name(&self) -> &str {
        &self.core.name
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        VirtioMmio::read(self, offset, size)
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        VirtioMmio::write(self, offset, size, val);
    }
}
