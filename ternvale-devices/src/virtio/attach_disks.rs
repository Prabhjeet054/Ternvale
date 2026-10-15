//! Attach config disk images as virtio-blk devices (virtio-mmio or virtio-pci).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ternvale_config::{BlockDevice, BlockRole, VirtioTransport};
use ternvale_vmm::pci::{Bdf, PciIdentity};
use ternvale_vmm::{DeviceAttach, MachineError, MmioDevice};

use super::blk::{BlkMedia, BlkStats};
use super::VirtioBlk;

/// Base, size, and MMIO device for one attached disk.
pub type AttachedDisk = (u64, u64, Box<dyn MmioDevice>);

/// Where [`attach_blocks`] put one device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlkPlacement {
    /// virtio-mmio slot.
    Mmio {
        /// Slot index (base `slot_base(slot)`).
        slot: u32,
    },
    /// virtio-pci function, with the identity the guest enumerates.
    Pci {
        /// Bus address.
        bdf: Bdf,
        /// IDs read back through config space.
        identity: Option<PciIdentity>,
    },
}

/// One device [`attach_blocks`] attached.
pub struct AttachedBlock {
    /// Its config role.
    pub role: BlockRole,
    /// Host image.
    pub path: PathBuf,
    /// What the guest was told it is.
    pub media: BlkMedia,
    /// Where it went.
    pub placement: BlkPlacement,
    /// Live request counters.
    pub stats: Arc<BlkStats>,
}

/// What [`attach_blocks`] produced.
pub struct AttachedBlocks {
    /// virtio-mmio devices for the attach callback to return (empty on virtio-pci).
    pub mmio: Vec<AttachedDisk>,
    /// Every device, in config order.
    pub devices: Vec<AttachedBlock>,
}

/// Attach a config's block devices ([`ternvale_config::VmConfig::block_devices`])
/// in order on `transport`: virtio-mmio slots `0..n`, or PCI 00:01.0 onward
/// (the slots the fw_cfg `bootorder` names). `[iso]` slots get
/// [`BlkMedia::Optical`]. Each device's guest-visible identity is logged at
/// INFO.
#[tracing::instrument(level = "debug", target = "ternvale::virtio::blk", skip_all, fields(devices = devices.len(), %transport))]
pub fn attach_blocks(
    attach: &DeviceAttach,
    devices: &[BlockDevice],
    transport: VirtioTransport,
) -> Result<AttachedBlocks, MachineError> {
    let mut out = AttachedBlocks {
        mmio: Vec::new(),
        devices: Vec::with_capacity(devices.len()),
    };
    for (index, device) in devices.iter().enumerate() {
        let media = if device.is_optical() {
            BlkMedia::Optical
        } else {
            BlkMedia::Disk {
                read_only: device.read_only,
            }
        };
        let fail = |error: String| {
            tracing::error!(target: "ternvale::virtio::blk", index, role = %device.role, path = %device.path.display(), error = %error, "block device attach failed");
            MachineError::Attach(format!(
                "{} {index} ({}): {error}",
                device.role,
                device.path.display()
            ))
        };
        let (placement, stats) = match transport {
            VirtioTransport::Pci => {
                let (bdf, stats) = VirtioBlk::attach_pci_media(attach, &device.path, media)
                    .map_err(|error| fail(error.to_string()))?;
                if ternvale_vmm::pci_disk_bdf(index) != Some(bdf) {
                    tracing::warn!(target: "ternvale::virtio::blk", index, %bdf, "block device is not on the PCI slot the uefi boot order names");
                }
                let identity = PciIdentity::read(&attach.pci, bdf);
                (BlkPlacement::Pci { bdf, identity }, stats)
            }
            VirtioTransport::Mmio => {
                let slot =
                    u32::try_from(index).map_err(|_| fail("slot does not fit in u32".into()))?;
                let (base, size, mmio, stats) = VirtioBlk::attach_media(
                    slot,
                    &device.path,
                    media,
                    Arc::clone(&attach.memory),
                    attach.virtio_irq_hook(slot),
                )
                .map_err(|error| fail(error.to_string()))?;
                out.mmio.push((base, size, mmio));
                (BlkPlacement::Mmio { slot }, stats)
            }
        };
        let block = AttachedBlock {
            role: device.role,
            path: device.path.clone(),
            media,
            placement,
            stats,
        };
        log_identity(&block);
        out.devices.push(block);
    }
    Ok(out)
}

/// INFO: which image is behind which guest-visible device, and the IDs the
/// guest (EDK2's `PciBus` log, Windows' hardware IDs, `lspci`) will report.
fn log_identity(block: &AttachedBlock) {
    let media = block.media.name();
    let features = format!("{:#x}", block.media.features());
    match &block.placement {
        BlkPlacement::Pci {
            bdf,
            identity: Some(id),
        } => tracing::info!(
            target: "ternvale::virtio::blk",
            role = %block.role,
            %bdf,
            pci_id = %id,
            subsystem = format!("{:04x}:{:04x}", id.subsystem_vendor_id, id.subsystem_id),
            class = format!("{:#08x}", id.class_code),
            revision = id.revision,
            windows_hwid = %id.windows_hardware_id(),
            edk2 = %id.edk2_discovered(),
            media,
            read_only = block.media.read_only(),
            blk_size = ?block.media.block_size(),
            features,
            path = %block.path.display(),
            "guest-visible block device"
        ),
        BlkPlacement::Pci {
            bdf,
            identity: None,
        } => tracing::warn!(
            target: "ternvale::virtio::blk",
            role = %block.role,
            %bdf,
            path = %block.path.display(),
            "attached pci block device does not answer config reads"
        ),
        BlkPlacement::Mmio { slot } => tracing::info!(
            target: "ternvale::virtio::blk",
            role = %block.role,
            slot,
            base = ?super::slot_base(*slot).map(|base| format!("{base:#x}")),
            virtio_device_id = super::blk::VIRTIO_BLK_ID,
            media,
            read_only = block.media.read_only(),
            blk_size = ?block.media.block_size(),
            features,
            path = %block.path.display(),
            "guest-visible block device"
        ),
    }
}

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
