//! Guest physical layout, modeled on QEMU's `virt` machine.
//!
//! Addresses that differ from QEMU are called out in `docs/ARCHITECTURE.md`.
//! Every region base and size is a multiple of [`HOST_PAGE_SIZE`] (16 KiB).

use crate::memory::HOST_PAGE_SIZE;

/// First byte of guest RAM. QEMU `VIRT_MEM`.
pub const RAM_BASE: u64 = 0x4000_0000;
/// Firmware flash at GPA 0. QEMU `VIRT_FLASH` is 128 MiB.
pub const FLASH_BASE: u64 = 0;
/// Size of the firmware flash window.
pub const FLASH_SIZE: u64 = 0x0800_0000;
/// GICv3 distributor. QEMU `VIRT_GIC_DIST`.
pub const GIC_DIST_BASE: u64 = 0x0800_0000;
/// Distributor window. QEMU uses 64 KiB.
pub const GIC_DIST_SIZE: u64 = 0x0001_0000;
/// GICv3 redistributor. QEMU `VIRT_GIC_REDIST`.
pub const GIC_REDIST_BASE: u64 = 0x080a_0000;
/// Redistributor window, ending at the UART.
pub const GIC_REDIST_SIZE: u64 = 0x00f6_0000;
/// PL011 UART. QEMU `VIRT_UART`.
pub const UART_BASE: u64 = 0x0900_0000;
/// UART window. QEMU's device is 4 KiB; this reservation is one host page.
pub const UART_SIZE: u64 = HOST_PAGE_SIZE;
/// PL031 RTC. QEMU `VIRT_RTC`.
pub const RTC_BASE: u64 = 0x0901_0000;
/// RTC window. QEMU's device is 4 KiB; this reservation is one host page.
pub const RTC_SIZE: u64 = HOST_PAGE_SIZE;
/// First virtio-mmio slot. QEMU `VIRT_MMIO`.
pub const VIRTIO_MMIO_BASE: u64 = 0x0a00_0000;
/// Slots in the virtio-mmio window.
pub const VIRTIO_MMIO_SLOTS: u64 = 32;
/// Bytes per virtio-mmio slot. QEMU uses `0x200`.
pub const VIRTIO_MMIO_SLOT_SIZE: u64 = 0x200;
/// Whole virtio-mmio window: 32 slots of `0x200`.
pub const VIRTIO_MMIO_SIZE: u64 = VIRTIO_MMIO_SLOTS * VIRTIO_MMIO_SLOT_SIZE;
/// Low PCIe MMIO. QEMU `VIRT_PCIE_MMIO` starts here.
pub const PCIE_MMIO_BASE: u64 = 0x1000_0000;
/// Low PCIe MMIO size. Runs up to ECAM. See the architecture note.
pub const PCIE_MMIO_SIZE: u64 = 0x2f00_0000;
/// Low PCIe ECAM. QEMU `VIRT_PCIE_ECAM`.
pub const PCIE_ECAM_BASE: u64 = 0x3f00_0000;
/// Low ECAM size. QEMU uses 16 MiB.
pub const PCIE_ECAM_SIZE: u64 = 0x0100_0000;

/// One guest-physical window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Short name used in logs.
    pub name: &'static str,
    /// First guest physical byte.
    pub base: u64,
    /// Length in bytes.
    pub size: u64,
}

/// The guest physical map for one VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    regions: Vec<Region>,
}

/// The map failed validation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlatformError {
    /// A base or size is not a multiple of the host page size.
    #[error("{name} base {base:#x} size {size:#x} is not aligned to {align:#x}")]
    Misaligned {
        /// Region name.
        name: &'static str,
        /// Region base.
        base: u64,
        /// Region length.
        size: u64,
        /// Required alignment.
        align: u64,
    },
    /// Two windows share bytes.
    #[error(
        "{name} {base:#x} size {size:#x} overlaps {other} {other_base:#x} size {other_size:#x}"
    )]
    Overlap {
        /// Region that was checked second.
        name: &'static str,
        /// Its base.
        base: u64,
        /// Its length.
        size: u64,
        /// Region already accepted.
        other: &'static str,
        /// The other base.
        other_base: u64,
        /// The other length.
        other_size: u64,
    },
}

impl Layout {
    /// QEMU `virt` map with `ram_bytes` of RAM at [`RAM_BASE`].
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::boot",
        skip_all,
        fields(ram_bytes)
    )]
    pub fn virt(ram_bytes: u64) -> Self {
        let regions = vec![
            region("flash", FLASH_BASE, FLASH_SIZE),
            region("gic-dist", GIC_DIST_BASE, GIC_DIST_SIZE),
            region("gic-redist", GIC_REDIST_BASE, GIC_REDIST_SIZE),
            region("uart", UART_BASE, UART_SIZE),
            region("rtc", RTC_BASE, RTC_SIZE),
            region("virtio-mmio", VIRTIO_MMIO_BASE, VIRTIO_MMIO_SIZE),
            region("pcie-mmio", PCIE_MMIO_BASE, PCIE_MMIO_SIZE),
            region("pcie-ecam", PCIE_ECAM_BASE, PCIE_ECAM_SIZE),
            region("ram", RAM_BASE, ram_bytes),
        ];
        Self { regions }
    }

    /// Regions in ascending base order, as passed to [`Layout::virt`].
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// Reject a zero size, a base or size that is not 16 KiB aligned, or an overlap.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn validate(&self) -> Result<(), PlatformError> {
        for region in &self.regions {
            if region.size == 0
                || region.base % HOST_PAGE_SIZE != 0
                || region.size % HOST_PAGE_SIZE != 0
            {
                let error = PlatformError::Misaligned {
                    name: region.name,
                    base: region.base,
                    size: region.size,
                    align: HOST_PAGE_SIZE,
                };
                tracing::warn!(target: "ternvale::boot", error = %error, "rejected guest layout");
                return Err(error);
            }
        }
        for (index, region) in self.regions.iter().enumerate() {
            let end = match region.base.checked_add(region.size) {
                Some(end) => end,
                None => {
                    let error = PlatformError::Misaligned {
                        name: region.name,
                        base: region.base,
                        size: region.size,
                        align: HOST_PAGE_SIZE,
                    };
                    tracing::warn!(target: "ternvale::boot", error = %error, "rejected guest layout");
                    return Err(error);
                }
            };
            for other in &self.regions[..index] {
                let other_end = other.base + other.size;
                if region.base < other_end && other.base < end {
                    let error = PlatformError::Overlap {
                        name: region.name,
                        base: region.base,
                        size: region.size,
                        other: other.name,
                        other_base: other.base,
                        other_size: other.size,
                    };
                    tracing::warn!(target: "ternvale::boot", error = %error, "rejected guest layout");
                    return Err(error);
                }
            }
        }
        tracing::debug!(target: "ternvale::boot", regions = self.regions.len(), "guest layout accepted");
        Ok(())
    }

    /// Log every region at INFO. Call this when the VM starts.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn dump(&self) {
        tracing::info!(target: "ternvale::boot", regions = self.regions.len(), "guest physical map");
        for region in &self.regions {
            tracing::info!(
                target: "ternvale::boot",
                name = region.name,
                base = format!("{:#x}", region.base),
                size = format!("{:#x}", region.size),
                end = format!("{:#x}", region.base.saturating_add(region.size)),
                "guest region"
            );
        }
    }
}

fn region(name: &'static str, base: u64, size: u64) -> Region {
    Region { name, base, size }
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod tests;
