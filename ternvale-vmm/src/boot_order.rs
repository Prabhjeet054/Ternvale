//! fw_cfg `bootorder`: the UEFI boot order for profiles that boot the
//! attached ISO first ([`VmConfig::iso_boots_first`]).
//!
//! EDK2 ArmVirtQemu reads it in `PlatformBootManagerAfterConsole`
//! (`OvmfPkg/Library/PlatformBootManagerLibLight/PlatformBm.c`) through
//! `SetBootOrderFromQemu` in `OvmfPkg/Library/QemuBootOrderLib/QemuBootOrderLib.c`
//! (edk2 master, read 2026-10-13; `ArmVirtQemu.dsc` sets
//! `PcdQemuBootOrderPciTranslation` and `PcdQemuBootOrderMmioTranslation`):
//! - One OpenFirmware device path per line (`\n`, IEEE 1275 node names); the
//!   file must end in NUL.
//! - A virtio-blk PCI function: `/pci@i0cf8/scsi@<slot>[,<fn>]/disk@0,0`,
//!   translated to `PciRoot(0x0)/Pci(<slot>,<fn>)`. Only the first node's
//!   driver name `pci` is checked, not its unit address.
//! - A virtio-mmio disk: `/virtio-mmio@<base>/disk@0,0`, translated to the
//!   transport's `VenHw(<virtio-mmio GUID>, <base>)`.
//! - Each translated path selects the boot options it prefixes, in file order.
//!   Options under `PciRoot(0x0)` or a virtio-mmio `VenHw` that no line selects
//!   are dropped from `BootOrder` (`BootOrderComplete`), so every bootable
//!   device is listed: `iso.installer`, then ISO disks, then the other disks
//!   in config order. `iso.drivers` is deliberately unlisted, which drops its
//!   option. The UEFI shell and other firmware-volume options keep their place
//!   after them.
//!
//! TODO(verify): EDK2 builds may differ. The `windows-profile` boot scenario
//! checks that `BdsDxe` loads the ISO first even though it is the second disk.

use std::path::Path;

use ternvale_config::{BlockRole, VirtioTransport, VmConfig};

use crate::machine::MachineError;
use crate::pci::Bdf;
use crate::platform::{VIRTIO_MMIO_BASE, VIRTIO_MMIO_SLOTS, VIRTIO_MMIO_SLOT_SIZE};

/// fw_cfg file name EDK2 looks up (QEMU's `-boot order` / `bootindex` file).
pub const BOOTORDER_FILE: &str = "bootorder";
/// PCI devices per bus.
const PCI_DEVICES: usize = 32;

/// The PCI function block device `index` ([`VmConfig::block_devices`]) gets
/// when block devices are attached first, in order, behind the host bridge
/// at 00:00.0. `None` past the last device.
#[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(index))]
pub fn pci_disk_bdf(index: usize) -> Option<Bdf> {
    let device = index
        .checked_add(1)
        .filter(|device| *device < PCI_DEVICES)?;
    u8::try_from(device).ok().map(Bdf::new)
}

/// The OpenFirmware path of config disk `index` on `transport`.
#[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(%transport, index))]
pub fn disk_ofw_path(transport: VirtioTransport, index: usize) -> Result<String, MachineError> {
    let path = match transport {
        VirtioTransport::Pci => {
            pci_disk_bdf(index).map(|bdf| format!("/pci@i0cf8/scsi@{:x}/disk@0,0", bdf.device))
        }
        VirtioTransport::Mmio => u64::try_from(index)
            .ok()
            .filter(|slot| *slot < VIRTIO_MMIO_SLOTS)
            .map(|slot| {
                let base = VIRTIO_MMIO_BASE + slot * VIRTIO_MMIO_SLOT_SIZE;
                format!("/virtio-mmio@{base:016x}/disk@0,0")
            }),
    };
    path.ok_or_else(|| {
        tracing::error!(target: "ternvale::boot", %transport, index, "disk has no boot order path");
        MachineError::Attach(format!("disk {index} does not fit on {transport}"))
    })
}

/// `(name, contents)` of the `bootorder` file for `config`, or `None` when
/// the profile keeps EDK2's own order or nothing bootable is an ISO image.
///
/// Indices are positions in [`VmConfig::block_devices`]: `iso.installer`
/// first, then `[[disks]]` that are ISO images, then the other disks.
/// `iso.drivers` is left out, so EDK2 drops its boot option.
#[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(name = %config.name, profile = %config.profile))]
pub(crate) fn file(config: &VmConfig) -> Result<Option<(String, Vec<u8>)>, MachineError> {
    if !config.iso_boots_first() {
        tracing::debug!(target: "ternvale::boot", "profile keeps the firmware boot order");
        return Ok(None);
    }
    let devices = config.block_devices();
    let mut isos = Vec::new();
    let mut others = Vec::new();
    for (index, device) in devices.iter().enumerate() {
        match device.role {
            BlockRole::Installer => isos.insert(0, index),
            BlockRole::Drivers => {
                tracing::info!(target: "ternvale::boot", index, path = %device.path.display(), "driver disc left out of the uefi boot order");
            }
            BlockRole::Disk if is_iso(&device.path)? => isos.push(index),
            BlockRole::Disk => others.push(index),
        }
    }
    if isos.is_empty() {
        tracing::info!(target: "ternvale::boot", devices = devices.len(), "no ISO attached; EDK2 keeps its own boot order");
        return Ok(None);
    }
    let transport = config.virtio_transport();
    let paths = isos
        .iter()
        .chain(&others)
        .map(|index| disk_ofw_path(transport, *index))
        .collect::<Result<Vec<_>, _>>()?;
    let mut bytes = paths.join("\n").into_bytes();
    bytes.push(0);
    tracing::info!(
        target: "ternvale::boot",
        %transport,
        isos = ?isos,
        order = %paths.join(" "),
        "uefi boot order: attached ISO first"
    );
    Ok(Some((BOOTORDER_FILE.to_string(), bytes)))
}

fn is_iso(path: &Path) -> Result<bool, MachineError> {
    ternvale_config::is_iso9660(path).map_err(|source| {
        tracing::error!(target: "ternvale::boot", path = %path.display(), error = %source, "could not probe disk for ISO 9660");
        MachineError::Read {
            what: "disk",
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(test)]
#[path = "boot_order_tests.rs"]
mod tests;
