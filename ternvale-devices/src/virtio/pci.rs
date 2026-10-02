//! Modern (virtio 1.x) virtio-pci transport.
//!
//! One function per device: vendor 0x1af4, device `0x1040 + virtio id`,
//! revision 1. BAR 0 (32-bit, 16 KiB) holds the common, ISR, device, and
//! notify structures described by vendor capabilities ([`caps`]); a
//! `pci_cfg` capability gives config-space access to the same BAR. Interrupts
//! use INTA: the pin is high while the ISR is non-zero, and reading the ISR
//! clears it. MSI-X is an optional logged stub ([`Msix`]), off by default.

mod caps;
mod common;

use std::sync::Arc;

use ternvale_vmm::pci::{
    Bar, BarKind, Bdf, ConfigSpace, Header, IntxPin, Msix, PciError, PciFunction, MSIX_NO_VECTOR,
};
use ternvale_vmm::{DeviceAttach, MachineError};

use super::core::{mask, VirtioCore};
use super::irq::VirtioIrq;
use super::VirtioDevice;
use caps::{PciCfgCap, BAR_SIZE, BAR_SIZE_MSIX, COMMON_OFFSET, DEVICE_OFFSET, ISR_OFFSET};
use caps::{MSIX_PBA_OFFSET, MSIX_TABLE_OFFSET, NOTIFY_MULTIPLIER, NOTIFY_OFFSET, REGION_SIZE};

/// Red Hat / Qumranet, the virtio PCI vendor.
pub const VIRTIO_PCI_VENDOR_ID: u16 = 0x1af4;
/// Modern device IDs are `0x1040 + virtio device id`.
pub const VIRTIO_PCI_DEVICE_BASE: u16 = 0x1040;

/// Virtio device behind one PCI function.
pub struct VirtioPci {
    core: VirtioCore,
    config: ConfigSpace,
    pci_cfg: PciCfgCap,
    dfselect: u32,
    gfselect: u32,
    queue_select: u16,
    msix_config: u16,
    queue_vectors: Vec<u16>,
    msix: Option<Msix>,
}

/// Class code QEMU uses for the device type; 0xff0000 (unclassified) otherwise.
fn class_code(device_id: u32) -> u32 {
    match device_id {
        1 => 0x02_00_00,
        2 => 0x01_00_00,
        _ => 0xff_00_00,
    }
}

impl VirtioPci {
    /// Build function `bdf` for `device`. `interrupt` is shared with the device
    /// worker and drives `pin`. `msix_vectors > 0` adds the MSI-X stub.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::pci",
        skip_all,
        fields(%bdf, device_id = device.device_id(), msix_vectors)
    )]
    pub fn new(
        bdf: Bdf,
        device: Box<dyn VirtioDevice>,
        interrupt: Arc<VirtioIrq>,
        pin: Arc<IntxPin>,
        msix_vectors: u16,
    ) -> Result<Self, PciError> {
        let name = format!("virtio-pci-{bdf}");
        let device_id = device.device_id();
        let mut config = ConfigSpace::new(Header {
            vendor_id: VIRTIO_PCI_VENDOR_ID,
            device_id: VIRTIO_PCI_DEVICE_BASE + device_id as u16,
            class_code: class_code(device_id),
            revision: 1,
            subsystem_vendor_id: VIRTIO_PCI_VENDOR_ID,
            subsystem_id: 0x1100,
            interrupt_pin: 1,
        });
        let size = if msix_vectors > 0 {
            BAR_SIZE_MSIX
        } else {
            BAR_SIZE
        };
        config.add_bar(
            usize::from(caps::BAR),
            Bar {
                size,
                kind: BarKind::Mem32,
                prefetchable: false,
            },
        )?;
        let pci_cfg = caps::add_all(&mut config)?;
        let msix = match msix_vectors {
            0 => None,
            vectors => {
                let mut msix = Msix::new(
                    name.clone(),
                    vectors,
                    caps::BAR,
                    MSIX_TABLE_OFFSET,
                    MSIX_PBA_OFFSET,
                );
                msix.add_capability(&mut config)?;
                Some(msix)
            }
        };
        let weak = Arc::downgrade(&interrupt);
        interrupt.set_hook(Arc::new(move |_| {
            pin.refresh(&|| weak.upgrade().is_some_and(|irq| irq.status() != 0));
        }));
        let queues = device.num_queues();
        tracing::info!(
            target: "ternvale::virtio::pci",
            name = %name,
            device_id,
            pci_device = format!("{:#06x}", VIRTIO_PCI_DEVICE_BASE + device_id as u16),
            queues,
            bar_size = format!("{size:#x}"),
            msix_vectors,
            "virtio-pci transport created"
        );
        Ok(Self {
            core: VirtioCore::new(name, device, interrupt),
            config,
            pci_cfg,
            dfselect: 0,
            gfselect: 0,
            queue_select: 0,
            msix_config: MSIX_NO_VECTOR,
            queue_vectors: vec![MSIX_NO_VECTOR; usize::from(queues)],
            msix,
        })
    }

    /// Shared interrupt line.
    pub fn irq(&self) -> Arc<VirtioIrq> {
        Arc::clone(&self.core.interrupt)
    }

    fn bar0_read(&mut self, offset: u64, size: u8) -> u64 {
        if let Some(msix) = self.msix.as_ref().filter(|m| m.claims(0, offset)) {
            return msix.read(offset, size);
        }
        let rel = offset % REGION_SIZE;
        match offset - rel {
            COMMON_OFFSET => self.common_read(rel, size),
            ISR_OFFSET if rel == 0 => {
                let bits = self.core.interrupt.status();
                self.core.interrupt.ack(bits);
                u64::from(bits)
            }
            DEVICE_OFFSET => self.core.device.read_config(rel, size),
            _ => 0,
        }
    }

    fn bar0_write(&mut self, offset: u64, size: u8, value: u64) {
        if let Some(msix) = self.msix.as_mut().filter(|m| m.claims(0, offset)) {
            msix.write(offset, size, value);
            return;
        }
        let rel = offset % REGION_SIZE;
        match offset - rel {
            COMMON_OFFSET => self.common_write(rel, size, value),
            DEVICE_OFFSET => self.core.device.write_config(rel, size, value),
            NOTIFY_OFFSET if rel % u64::from(NOTIFY_MULTIPLIER) == 0 => {
                let queue = (rel / u64::from(NOTIFY_MULTIPLIER)) as u32;
                tracing::trace!(target: "ternvale::virtio::pci", name = %self.core.name, queue, value, "virtio-pci notify");
                self.core.notify(queue);
            }
            _ => tracing::warn!(
                target: "ternvale::virtio::pci",
                name = %self.core.name,
                offset = format!("{offset:#x}"),
                size,
                value = format!("{value:#x}"),
                "write to read-only or unknown virtio-pci bar offset"
            ),
        }
    }

    /// `pci_cfg` window: (bar, offset, length) as programmed, if usable.
    fn pci_cfg_target(&self) -> Option<(u64, u8)> {
        let (bar, offset, length) = self.pci_cfg.window(&self.config);
        let ok = bar == caps::BAR && matches!(length, 1 | 2 | 4) && offset % length == 0;
        if !ok && length == 0 {
            // Whole-config dumps (lspci -v, sysfs `config`) read pci_cfg_data unprogrammed.
            tracing::debug!(target: "ternvale::virtio::pci", name = %self.core.name, bar, offset, "pci_cfg data accessed with no window programmed");
            return None;
        }
        if !ok {
            tracing::warn!(target: "ternvale::virtio::pci", name = %self.core.name, bar, offset, length, "invalid pci_cfg window");
            return None;
        }
        Some((u64::from(offset), length as u8))
    }

    fn trace(
        &self,
        space: &'static str,
        offset: u64,
        size: u8,
        value: u64,
        direction: &'static str,
    ) {
        tracing::trace!(
            target: "ternvale::virtio::pci",
            name = %self.core.name,
            space,
            offset = format!("{offset:#x}"),
            size,
            value = format!("{value:#x}"),
            direction,
            "virtio-pci access"
        );
    }
}

impl PciFunction for VirtioPci {
    fn name(&self) -> &str {
        &self.core.name
    }

    fn config(&self) -> &ConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut ConfigSpace {
        &mut self.config
    }

    fn read_config(&mut self, offset: u16, size: u8) -> u32 {
        if usize::from(offset) == self.pci_cfg.data() {
            let value = match self.pci_cfg_target() {
                Some((bar_offset, len)) => mask(self.bar0_read(bar_offset, len), len) as u32,
                None => 0,
            };
            self.trace("pci_cfg", u64::from(offset), size, u64::from(value), "read");
            return mask(u64::from(value), size) as u32;
        }
        self.config.read(offset, size)
    }

    fn write_config(&mut self, offset: u16, size: u8, value: u32) {
        if usize::from(offset) == self.pci_cfg.data() {
            self.trace(
                "pci_cfg",
                u64::from(offset),
                size,
                u64::from(value),
                "write",
            );
            if let Some((bar_offset, len)) = self.pci_cfg_target() {
                self.bar0_write(bar_offset, len, mask(u64::from(value), len));
            }
            return;
        }
        self.config.write(offset, size, value);
        if let Some(msix) = &mut self.msix {
            msix.sync(&self.config);
        }
    }

    fn read_bar(&mut self, bar: usize, offset: u64, size: u8) -> u64 {
        let value = if bar == usize::from(caps::BAR) {
            self.bar0_read(offset, size)
        } else {
            u64::MAX
        };
        self.trace("bar", offset, size, value, "read");
        mask(value, size)
    }

    fn write_bar(&mut self, bar: usize, offset: u64, size: u8, value: u64) {
        self.trace("bar", offset, size, value, "write");
        if bar == usize::from(caps::BAR) {
            self.bar0_write(offset, size, value);
        }
    }
}

/// Put `device` on the next free PCI slot. `interrupt` must be the line the
/// device worker raises.
#[tracing::instrument(level = "debug", target = "ternvale::virtio::pci", skip_all, fields(device_id = device.device_id()))]
pub fn attach_pci(
    attach: &DeviceAttach,
    device: Box<dyn VirtioDevice>,
    interrupt: Arc<VirtioIrq>,
) -> Result<Bdf, MachineError> {
    attach.add_pci(move |bdf, pin| {
        let function = VirtioPci::new(bdf, device, interrupt, pin, 0).map_err(|e| e.to_string())?;
        Ok(Box::new(function))
    })
}

#[cfg(test)]
#[path = "pci_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pci_blk_tests.rs"]
mod blk_tests;
