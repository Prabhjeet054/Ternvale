//! Attach config disk images as virtio-blk devices (virtio-mmio or virtio-pci).

use std::path::Path;
use std::sync::Arc;

use ternvale_vmm::pci::Bdf;
use ternvale_vmm::{DeviceAttach, MachineError, MmioDevice};

use super::VirtioBlk;

/// Base, size, and MMIO device for one attached disk.
pub type AttachedDisk = (u64, u64, Box<dyn MmioDevice>);

/// Open each `(path, read_only)` as virtio-blk on successive slots starting at 0.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtio::blk",
    skip_all,
    fields(disks = disks.len())
)]
pub fn attach_disks<P: AsRef<Path>>(
    attach: &DeviceAttach,
    disks: &[(P, bool)],
) -> Result<Vec<AttachedDisk>, MachineError> {
    let mut devices = Vec::with_capacity(disks.len());
    for (slot, (path, read_only)) in disks.iter().enumerate() {
        let slot = u32::try_from(slot)
            .map_err(|_| MachineError::Attach(format!("disk slot {slot} does not fit in u32")))?;
        let path = path.as_ref();
        tracing::info!(
            target: "ternvale::virtio::blk",
            slot,
            path = %path.display(),
            read_only = *read_only,
            "attaching virtio-blk from config disk"
        );
        let (base, size, device, _stats) = VirtioBlk::attach(
            slot,
            path,
            *read_only,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(slot),
        )
        .map_err(|error| MachineError::Attach(error.to_string()))?;
        devices.push((base, size, device));
    }
    Ok(devices)
}

/// Open each `(path, read_only)` as a virtio-blk PCI function, in order, on
/// the free device numbers of bus 0 (the first disk is 00:01.0).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtio::blk",
    skip_all,
    fields(disks = disks.len())
)]
pub fn attach_disks_pci<P: AsRef<Path>>(
    attach: &DeviceAttach,
    disks: &[(P, bool)],
) -> Result<Vec<Bdf>, MachineError> {
    let mut functions = Vec::with_capacity(disks.len());
    for (path, read_only) in disks {
        let path = path.as_ref();
        let (bdf, _stats) = VirtioBlk::attach_pci(attach, path, *read_only)?;
        tracing::info!(
            target: "ternvale::virtio::blk",
            %bdf,
            path = %path.display(),
            read_only = *read_only,
            "attached virtio-blk over pci from config disk"
        );
        functions.push(bdf);
    }
    Ok(functions)
}
