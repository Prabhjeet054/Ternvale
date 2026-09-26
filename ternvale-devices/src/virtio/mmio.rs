//! Virtio-mmio v2 register file.
//!
//! Status transitions live in [`status`]. Queue registers live in [`queue`].

mod queue;
mod status;

use ternvale_vmm::{
    MmioBus, MmioDevice, MmioError, VIRTIO_MMIO_BASE, VIRTIO_MMIO_SLOTS, VIRTIO_MMIO_SLOT_SIZE,
};

use super::{VirtioDevice, VIRTIO_F_VERSION_1};

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

#[derive(Debug)]
pub(super) struct Queue {
    num_max: u32,
    num: u32,
    ready: bool,
    desc: u64,
    driver: u64,
    device: u64,
}

/// Virtio-mmio v2 transport for one [`VirtioDevice`].
pub struct VirtioMmio {
    device: Box<dyn VirtioDevice>,
    name: String,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: u64,
    queue_sel: u32,
    queues: Vec<Queue>,
    status: u32,
    interrupt: u32,
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
        tracing::info!(
            target: "ternvale::virtio::mmio",
            slot,
            device_id = device.device_id(),
            queues = device.num_queues(),
            "virtio-mmio transport created"
        );
        Self {
            device,
            name: format!("virtio-mmio-{slot}"),
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: 0,
            queue_sel: 0,
            queues,
            status: 0,
            interrupt: 0,
        }
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
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::mmio", skip(self), fields(name = %self.name, bits))]
    pub fn raise_interrupt(&mut self, bits: u32) {
        let bits = bits & 0b11;
        self.interrupt |= bits;
        tracing::debug!(
            target: "ternvale::virtio::mmio",
            name = %self.name,
            interrupt = self.interrupt,
            "virtio interrupt raised"
        );
    }

    /// Guest read of one register or a config-space field.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::mmio",
        skip(self),
        fields(name = %self.name, offset = format!("{offset:#x}"), size)
    )]
    pub fn read(&mut self, offset: u64, size: u8) -> u64 {
        let value = if offset >= CONFIG {
            self.device.read_config(offset - CONFIG, size)
        } else if size != 4 {
            tracing::warn!(
                target: "ternvale::virtio::mmio",
                name = %self.name,
                offset = format!("{offset:#x}"),
                size,
                "virtio-mmio register access is not 4 bytes"
            );
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
        fields(name = %self.name, offset = format!("{offset:#x}"), size, value = format!("{value:#x}"))
    )]
    pub fn write(&mut self, offset: u64, size: u8, value: u64) {
        self.trace(offset, size, value, "write");
        if offset >= CONFIG {
            self.device.write_config(offset - CONFIG, size, value);
            return;
        }
        if size != 4 {
            tracing::warn!(
                target: "ternvale::virtio::mmio",
                name = %self.name,
                offset = format!("{offset:#x}"),
                size,
                "virtio-mmio register access is not 4 bytes"
            );
            return;
        }
        self.write_reg(offset, value as u32);
    }

    fn read_reg(&mut self, offset: u64) -> u64 {
        let value = match offset {
            MAGIC_VALUE => MAGIC,
            VERSION_REG => VERSION,
            DEVICE_ID => self.device.device_id(),
            VENDOR_ID => self.device.vendor_id(),
            DEVICE_FEATURES => feature_word(self.offered(), self.device_features_sel),
            QUEUE_NUM_MAX => self.selected().map(|q| q.num_max).unwrap_or(0),
            QUEUE_READY => u32::from(self.selected().is_some_and(|q| q.ready)),
            INTERRUPT_STATUS => self.interrupt,
            STATUS => self.status,
            CONFIG_GENERATION => self.device.config_generation(),
            _ => {
                tracing::warn!(
                    target: "ternvale::virtio::mmio",
                    name = %self.name,
                    offset = format!("{offset:#x}"),
                    "read of write-only or unknown virtio-mmio register"
                );
                0
            }
        };
        u64::from(value)
    }

    fn write_reg(&mut self, offset: u64, value: u32) {
        match offset {
            DEVICE_FEATURES_SEL => self.device_features_sel = value,
            DRIVER_FEATURES_SEL => self.driver_features_sel = value,
            DRIVER_FEATURES => self.write_driver_features(value),
            QUEUE_SEL => self.queue_sel = value,
            QUEUE_NUM => self.write_queue_num(value),
            QUEUE_READY => self.write_queue_ready(value),
            QUEUE_NOTIFY => self.write_notify(value),
            INTERRUPT_ACK => {
                self.interrupt &= !value;
                tracing::debug!(
                    target: "ternvale::virtio::mmio",
                    name = %self.name,
                    ack = value,
                    interrupt = self.interrupt,
                    "virtio interrupt ack"
                );
            }
            STATUS => self.write_status(value),
            QUEUE_DESC_LOW => self.write_addr(|q| &mut q.desc, false, value),
            QUEUE_DESC_HIGH => self.write_addr(|q| &mut q.desc, true, value),
            QUEUE_DRIVER_LOW => self.write_addr(|q| &mut q.driver, false, value),
            QUEUE_DRIVER_HIGH => self.write_addr(|q| &mut q.driver, true, value),
            QUEUE_DEVICE_LOW => self.write_addr(|q| &mut q.device, false, value),
            QUEUE_DEVICE_HIGH => self.write_addr(|q| &mut q.device, true, value),
            _ => tracing::warn!(
                target: "ternvale::virtio::mmio",
                name = %self.name,
                offset = format!("{offset:#x}"),
                value = format!("{value:#x}"),
                "write to read-only or unknown virtio-mmio register"
            ),
        }
    }

    pub(super) fn offered(&self) -> u64 {
        self.device.device_features() | VIRTIO_F_VERSION_1
    }

    pub(super) fn selected(&self) -> Option<&Queue> {
        self.queues.get(self.queue_sel as usize)
    }

    pub(super) fn selected_mut(&mut self) -> Option<&mut Queue> {
        self.queues.get_mut(self.queue_sel as usize)
    }

    pub(super) fn warn_queue(&self, message: &'static str) {
        tracing::warn!(
            target: "ternvale::virtio::mmio",
            name = %self.name,
            queue = self.queue_sel,
            message
        );
    }

    fn trace(&self, offset: u64, size: u8, value: u64, direction: &'static str) {
        tracing::trace!(
            target: "ternvale::virtio::mmio",
            name = %self.name,
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
        &self.name
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        VirtioMmio::read(self, offset, size)
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        VirtioMmio::write(self, offset, size, val);
    }
}

fn feature_word(features: u64, sel: u32) -> u32 {
    match sel {
        0 => features as u32,
        1 => (features >> 32) as u32,
        _ => 0,
    }
}

fn mask(value: u64, size: u8) -> u64 {
    match size {
        1 => value & 0xff,
        2 => value & 0xffff,
        4 => value & 0xffff_ffff,
        _ => value,
    }
}
