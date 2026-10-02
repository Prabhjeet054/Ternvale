//! `struct virtio_pci_common_cfg` (virtio 1.2 section 4.1.4.3).

use ternvale_vmm::pci::MSIX_NO_VECTOR;

use super::VirtioPci;
use crate::virtio::core::{feature_word, QueueAddr};

const DFSELECT: u64 = 0x00;
const DFEATURE: u64 = 0x04;
const GFSELECT: u64 = 0x08;
const GFEATURE: u64 = 0x0c;
const MSIX_CONFIG: u64 = 0x10;
const NUM_QUEUES: u64 = 0x12;
const DEVICE_STATUS: u64 = 0x14;
const CONFIG_GENERATION: u64 = 0x15;
const QUEUE_SELECT: u64 = 0x16;
const QUEUE_SIZE: u64 = 0x18;
const QUEUE_MSIX_VECTOR: u64 = 0x1a;
const QUEUE_ENABLE: u64 = 0x1c;
const QUEUE_NOTIFY_OFF: u64 = 0x1e;
const QUEUE_DESC: u64 = 0x20;
const QUEUE_DRIVER: u64 = 0x28;
const QUEUE_DEVICE: u64 = 0x30;
const END: u64 = 0x38;

/// Ring address field and half for an offset in 0x20..0x38.
fn addr_field(offset: u64) -> (QueueAddr, bool) {
    let which = match offset {
        QUEUE_DESC..QUEUE_DRIVER => QueueAddr::Desc,
        QUEUE_DRIVER..QUEUE_DEVICE => QueueAddr::Driver,
        _ => QueueAddr::Device,
    };
    (which, offset % 8 == 4)
}

fn field_size(offset: u64) -> Option<u8> {
    match offset {
        DFSELECT | DFEATURE | GFSELECT | GFEATURE => Some(4),
        MSIX_CONFIG | NUM_QUEUES | QUEUE_SELECT | QUEUE_SIZE | QUEUE_MSIX_VECTOR | QUEUE_ENABLE
        | QUEUE_NOTIFY_OFF => Some(2),
        DEVICE_STATUS | CONFIG_GENERATION => Some(1),
        QUEUE_DESC..END if offset % 4 == 0 => Some(4),
        _ => None,
    }
}

impl VirtioPci {
    /// Access sizes must match the field, except 8-byte ring address accesses.
    fn common_size_ok(&self, offset: u64, size: u8, direction: &'static str) -> bool {
        let wide = size == 8 && (QUEUE_DESC..END).contains(&offset) && offset % 8 == 0;
        if wide || field_size(offset) == Some(size) {
            return true;
        }
        tracing::warn!(
            target: "ternvale::virtio::pci",
            name = %self.core.name,
            offset = format!("{offset:#x}"),
            size,
            direction,
            "virtio-pci common cfg access does not match a field"
        );
        false
    }

    pub(super) fn common_read(&mut self, offset: u64, size: u8) -> u64 {
        if !self.common_size_ok(offset, size, "read") {
            return 0;
        }
        let sel = u32::from(self.queue_select);
        let queue = self.core.queue(sel);
        match offset {
            DFSELECT => u64::from(self.dfselect),
            DFEATURE => u64::from(feature_word(self.core.offered(), self.dfselect)),
            GFSELECT => u64::from(self.gfselect),
            GFEATURE => u64::from(feature_word(self.core.driver_features, self.gfselect)),
            MSIX_CONFIG => u64::from(self.msix_config),
            NUM_QUEUES => self.core.queues.len() as u64,
            DEVICE_STATUS => u64::from(self.core.status),
            CONFIG_GENERATION => u64::from(self.core.device.config_generation() as u8),
            QUEUE_SELECT => u64::from(self.queue_select),
            // Before the driver picks a size the register reports the maximum.
            QUEUE_SIZE => {
                queue.map_or(0, |q| u64::from(if q.num == 0 { q.num_max } else { q.num }))
            }
            QUEUE_MSIX_VECTOR => self
                .queue_vectors
                .get(sel as usize)
                .map_or(u64::from(MSIX_NO_VECTOR), |&v| u64::from(v)),
            QUEUE_ENABLE => u64::from(queue.is_some_and(|q| q.ready)),
            QUEUE_NOTIFY_OFF => u64::from(self.queue_select),
            QUEUE_DESC..END => {
                let Some(q) = queue else { return 0 };
                let (which, high) = addr_field(offset);
                let addr = match which {
                    QueueAddr::Desc => q.desc,
                    QueueAddr::Driver => q.driver,
                    QueueAddr::Device => q.device,
                };
                if size == 8 {
                    addr
                } else if high {
                    addr >> 32
                } else {
                    addr & 0xffff_ffff
                }
            }
            _ => 0,
        }
    }

    pub(super) fn common_write(&mut self, offset: u64, size: u8, value: u64) {
        if !self.common_size_ok(offset, size, "write") {
            return;
        }
        let sel = u32::from(self.queue_select);
        match offset {
            DFSELECT => self.dfselect = value as u32,
            GFSELECT => self.gfselect = value as u32,
            GFEATURE => self.core.write_driver_features(self.gfselect, value as u32),
            MSIX_CONFIG => self.msix_config = self.accept_vector(value as u16, "config"),
            DEVICE_STATUS => {
                if value == 0 {
                    self.reset_transport();
                }
                self.core.write_status(value as u32);
            }
            QUEUE_SELECT => self.queue_select = value as u16,
            QUEUE_SIZE => self.core.set_queue_num(sel, value as u32),
            QUEUE_MSIX_VECTOR => {
                let vector = self.accept_vector(value as u16, "queue");
                if let Some(slot) = self.queue_vectors.get_mut(sel as usize) {
                    *slot = vector;
                }
            }
            QUEUE_ENABLE => self.enable_queue(sel, value as u32),
            QUEUE_DESC..END if size == 8 => {
                let (which, _) = addr_field(offset);
                self.core.set_queue_addr(sel, which, false, value as u32);
                self.core
                    .set_queue_addr(sel, which, true, (value >> 32) as u32);
            }
            QUEUE_DESC..END => {
                let (which, high) = addr_field(offset);
                self.core.set_queue_addr(sel, which, high, value as u32);
            }
            _ => tracing::warn!(
                target: "ternvale::virtio::pci",
                name = %self.core.name,
                offset = format!("{offset:#x}"),
                value = format!("{value:#x}"),
                "write to read-only virtio-pci common cfg field"
            ),
        }
    }

    /// `queue_enable = 1`. A queue whose size was never written uses the maximum.
    fn enable_queue(&mut self, sel: u32, value: u32) {
        if value == 1 {
            if let Some(max) = self
                .core
                .queue(sel)
                .filter(|q| q.num == 0)
                .map(|q| q.num_max)
            {
                self.core.set_queue_num(sel, max);
            }
        }
        self.core.set_queue_ready(sel, value);
    }

    /// Without MSI-X every vector reads back as `NO_VECTOR`, which tells the
    /// driver to stay on INTx.
    fn accept_vector(&self, vector: u16, what: &'static str) -> u16 {
        let accepted = match &self.msix {
            Some(_) => vector,
            None => MSIX_NO_VECTOR,
        };
        if vector != MSIX_NO_VECTOR {
            tracing::debug!(
                target: "ternvale::virtio::pci",
                name = %self.core.name,
                what,
                vector,
                accepted,
                "virtio-pci msi-x vector"
            );
        }
        accepted
    }

    fn reset_transport(&mut self) {
        self.dfselect = 0;
        self.gfselect = 0;
        self.queue_select = 0;
        self.msix_config = MSIX_NO_VECTOR;
        self.queue_vectors.fill(MSIX_NO_VECTOR);
    }
}
