//! BAR0 layout and the virtio vendor capabilities that describe it.
//!
//! `struct virtio_pci_cap` (virtio 1.2 section 4.1.4): `cap_vndr` 0x09,
//! `cap_next`, `cap_len`, `cfg_type`, `bar`, `id`, two pad bytes, `offset`
//! le32, `length` le32. The notify capability appends
//! `notify_off_multiplier` le32; the `pci_cfg` capability appends a 4-byte
//! data window.

use ternvale_vmm::pci::{ConfigSpace, PciError};

/// PCI vendor-specific capability ID.
pub(super) const CAP_VENDOR: u8 = 0x09;
/// Common configuration.
pub(super) const CFG_COMMON: u8 = 1;
/// Queue notifications.
pub(super) const CFG_NOTIFY: u8 = 2;
/// ISR status.
pub(super) const CFG_ISR: u8 = 3;
/// Device-specific configuration.
pub(super) const CFG_DEVICE: u8 = 4;
/// Config-space window onto the BARs.
pub(super) const CFG_PCI: u8 = 5;

/// Every structure lives in BAR 0.
pub(super) const BAR: u8 = 0;
pub(super) const COMMON_OFFSET: u64 = 0x0000;
pub(super) const ISR_OFFSET: u64 = 0x1000;
pub(super) const DEVICE_OFFSET: u64 = 0x2000;
pub(super) const NOTIFY_OFFSET: u64 = 0x3000;
/// Each region is one 4 KiB page.
pub(super) const REGION_SIZE: u64 = 0x1000;
/// Queue `n` is notified at `NOTIFY_OFFSET + n * NOTIFY_MULTIPLIER`.
pub(super) const NOTIFY_MULTIPLIER: u32 = 4;
/// MSI-X table and PBA follow the virtio regions when MSI-X is on.
pub(super) const MSIX_TABLE_OFFSET: u32 = 0x4000;
pub(super) const MSIX_PBA_OFFSET: u32 = 0x5000;
/// BAR 0 size without and with MSI-X.
pub(super) const BAR_SIZE: u64 = 0x4000;
pub(super) const BAR_SIZE_MSIX: u64 = 0x8000;

/// Config-space offsets of the `pci_cfg` capability's mutable fields.
#[derive(Debug, Clone, Copy)]
pub(super) struct PciCfgCap {
    cap: usize,
}

impl PciCfgCap {
    /// Config offset of `pci_cfg_data`.
    pub(super) fn data(self) -> usize {
        self.cap + 16
    }

    /// `(bar, offset, length)` the driver last programmed.
    pub(super) fn window(self, config: &ConfigSpace) -> (u8, u32, u32) {
        (
            config.bytes(self.cap + 4, 1).first().copied().unwrap_or(0),
            config.u32_at(self.cap + 8),
            config.u32_at(self.cap + 12),
        )
    }
}

fn body(cfg_type: u8, cap_len: u8, offset: u64, length: u64) -> Vec<u8> {
    let mut body = vec![cap_len, cfg_type, BAR, 0, 0, 0];
    body.extend_from_slice(&(offset as u32).to_le_bytes());
    body.extend_from_slice(&(length as u32).to_le_bytes());
    body
}

/// Add common, notify, ISR, device, and `pci_cfg` capabilities, in that order.
pub(super) fn add_all(config: &mut ConfigSpace) -> Result<PciCfgCap, PciError> {
    config.add_capability(
        CAP_VENDOR,
        &body(CFG_COMMON, 16, COMMON_OFFSET, REGION_SIZE),
        &[],
    )?;
    let mut notify = body(CFG_NOTIFY, 20, NOTIFY_OFFSET, REGION_SIZE);
    notify.extend_from_slice(&NOTIFY_MULTIPLIER.to_le_bytes());
    config.add_capability(CAP_VENDOR, &notify, &[])?;
    config.add_capability(CAP_VENDOR, &body(CFG_ISR, 16, ISR_OFFSET, REGION_SIZE), &[])?;
    config.add_capability(
        CAP_VENDOR,
        &body(CFG_DEVICE, 16, DEVICE_OFFSET, REGION_SIZE),
        &[],
    )?;
    let mut pci_cfg = body(CFG_PCI, 20, 0, 0);
    pci_cfg.extend_from_slice(&[0; 4]);
    // The driver writes bar, offset, length, and the data window.
    let mut wmask = vec![0, 0, 0xff, 0, 0, 0];
    wmask.extend_from_slice(&[0xff; 12]);
    let cap = config.add_capability(CAP_VENDOR, &pci_cfg, &wmask)?;
    Ok(PciCfgCap {
        cap: usize::from(cap),
    })
}
