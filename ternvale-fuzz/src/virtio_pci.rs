//! virtio-pci config space and BAR 0.

use std::sync::Arc;

use ternvale_devices::{FixedDevice, VirtioIrq, VirtioPci};
use ternvale_vmm::pci::{Bdf, IntxLine, IntxPin, PciFunction};

use crate::quiet;

/// Random config and BAR accesses against a dummy virtio function.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let line = IntxLine::new(0, Arc::new(|_| {}));
    let pin = IntxPin::new("fuzz-pci".into(), line);
    let irq = VirtioIrq::new();
    let device = Box::new(FixedDevice::new(1, 2));
    let Ok(mut pci) = VirtioPci::new(Bdf::new(1), device, irq, pin, 0) else {
        return;
    };
    for chunk in data.chunks(8).take(64) {
        if chunk.len() < 4 {
            break;
        }
        let offset = u16::from_le_bytes([chunk[0], chunk[1]]);
        let size = match chunk[2] % 3 {
            0 => 1,
            1 => 2,
            _ => 4,
        };
        let value = u32::from_le_bytes([
            chunk.get(3).copied().unwrap_or(0),
            chunk.get(4).copied().unwrap_or(0),
            chunk.get(5).copied().unwrap_or(0),
            chunk.get(6).copied().unwrap_or(0),
        ]);
        if chunk[2] & 0x80 == 0 {
            let _ = pci.read_config(offset % 256, size);
        } else {
            pci.write_config(offset % 256, size, value);
        }
        let bar_off = u64::from(u16::from_le_bytes([
            chunk.get(4).copied().unwrap_or(0),
            chunk.get(5).copied().unwrap_or(0),
        ])) % 0x4000;
        if chunk[2] & 0x40 == 0 {
            let _ = pci.read_bar(0, bar_off, size);
        } else {
            pci.write_bar(0, bar_off, size, u64::from(value));
        }
    }
}
