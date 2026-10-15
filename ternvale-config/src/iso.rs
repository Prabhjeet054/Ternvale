//! `[iso]`: two read-only optical slots next to `[[disks]]`.
//!
//! - `installer`: the OS installer image (a Windows 11 Arm64 ISO, say). The
//!   `windows` profile puts it first in the UEFI boot order.
//! - `drivers`: a driver disc for the installer, meant for Red Hat's
//!   virtio-win ISO (`scripts/fetch-virtio-win.sh`; it is fetched, never
//!   bundled). It is never booted: the boot order leaves it out, so EDK2
//!   drops its boot option.
//!
//! Both are attached after the disks, in that order ([`VmConfig::block_devices`]),
//! always read-only, as virtio-blk devices with optical-media features (see
//! `ternvale-devices`, `BlkMedia::Optical`).

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::VmConfig;

/// Logical sector size of ISO 9660 and UDF optical media (ECMA-119 6.1.2;
/// ECMA-167 uses the medium's sector size, 2048 on DVD).
pub const OPTICAL_SECTOR_BYTES: u64 = 2048;

/// The `[iso]` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsoSection {
    /// Installer image; first in the boot order with `profile = "windows"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer: Option<PathBuf>,
    /// Driver disc (virtio-win) for the installer; never booted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drivers: Option<PathBuf>,
}

/// Why a block device is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockRole {
    /// A `[[disks]]` entry.
    Disk,
    /// `iso.installer`.
    Installer,
    /// `iso.drivers`.
    Drivers,
}

impl fmt::Display for BlockRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Disk => "disk",
            Self::Installer => "installer",
            Self::Drivers => "drivers",
        })
    }
}

/// One virtio-blk device the VM gets, in attach order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDevice {
    /// Where it came from in the config.
    pub role: BlockRole,
    /// Host image.
    pub path: PathBuf,
    /// Read-only to the guest (always true for the `[iso]` slots).
    pub read_only: bool,
}

impl BlockDevice {
    /// True for the `[iso]` slots, which get optical-media features.
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all, fields(role = %self.role))]
    pub fn is_optical(&self) -> bool {
        self.role != BlockRole::Disk
    }
}

impl IsoSection {
    /// `(field, path)` of each set slot, installer first.
    fn slots(&self) -> impl Iterator<Item = (&'static str, BlockRole, &Path)> {
        [
            (
                "iso.installer",
                BlockRole::Installer,
                self.installer.as_deref(),
            ),
            ("iso.drivers", BlockRole::Drivers, self.drivers.as_deref()),
        ]
        .into_iter()
        .filter_map(|(field, role, path)| path.map(|path| (field, role, path)))
    }

    /// Each slot must be an existing disc image not attached twice.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %config.name))]
    pub(crate) fn validate(&self, config: &VmConfig) -> Result<(), ConfigError> {
        for (field, role, path) in self.slots() {
            crate::vm::require_file(field, path)?;
            let format = disc_format(path).map_err(|source| {
                invalid(
                    field,
                    path,
                    format!("could not read the volume descriptors: {source}"),
                )
            })?;
            let Some(format) = format else {
                return Err(invalid(
                    field,
                    path,
                    "no ISO 9660 (CD001) or UDF (BEA01) volume descriptor at sector 16".into(),
                ));
            };
            let same = |other: &Path| same_file(path, other);
            if let Some(index) = config.disks.iter().position(|disk| same(&disk.path)) {
                return Err(invalid(
                    field,
                    path,
                    format!("already attached as disks[{index}]"),
                ));
            }
            if role == BlockRole::Drivers && self.installer.as_deref().is_some_and(same) {
                return Err(invalid(
                    field,
                    path,
                    "already attached as iso.installer".into(),
                ));
            }
            warn_partial_sector(field, path);
            tracing::debug!(target: "ternvale::config", field, path = %path.display(), format, "accepted iso slot");
        }
        Ok(())
    }
}

impl VmConfig {
    /// Every virtio-blk device in attach order: `[[disks]]` in config order,
    /// then `iso.installer`, then `iso.drivers`.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn block_devices(&self) -> Vec<BlockDevice> {
        let disks = self.disks.iter().map(|disk| BlockDevice {
            role: BlockRole::Disk,
            path: disk.path.clone(),
            read_only: disk.read_only,
        });
        let isos = self.iso.iter().flat_map(|iso| {
            iso.slots().map(|(_, role, path)| BlockDevice {
                role,
                path: path.to_path_buf(),
                read_only: true,
            })
        });
        let devices: Vec<_> = disks.chain(isos).collect();
        tracing::debug!(target: "ternvale::config", devices = devices.len(), "block devices");
        devices
    }
}

fn invalid(field: &'static str, path: &Path, reason: String) -> ConfigError {
    tracing::error!(target: "ternvale::config", field, path = %path.display(), reason = %reason, "rejected iso slot");
    ConfigError::InvalidIso {
        field,
        path: path.to_path_buf(),
        reason,
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The guest reads whole 2048-byte blocks; a trailing partial one is unseen.
fn warn_partial_sector(field: &'static str, path: &Path) {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() % OPTICAL_SECTOR_BYTES != 0 => tracing::warn!(
            target: "ternvale::config",
            field,
            path = %path.display(),
            bytes = meta.len(),
            "image is not a whole number of 2048-byte sectors; the guest will not see the tail"
        ),
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(target: "ternvale::config", field, path = %path.display(), error = %error, "could not stat iso image")
        }
    }
}

/// `"iso9660"` or `"udf"` when `path` starts a volume recognition sequence at
/// byte 32768 (sector 16 of 2048): each descriptor's standard identifier is
/// bytes 1 to 5. ISO 9660 uses `CD001` (ECMA-119 4th ed., 8.1); a UDF-only
/// image starts with `BEA01` (ECMA-167 3rd ed., 2/9.2), as do the extended
/// area descriptors after a bridge image's `CD001` set.
#[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(path = %path.display()))]
pub fn disc_format(path: &Path) -> std::io::Result<Option<&'static str>> {
    use std::io::{Read, Seek, SeekFrom};
    const DESCRIPTOR_SET: u64 = 16 * OPTICAL_SECTOR_BYTES;
    let mut file = std::fs::File::open(path)?;
    if file.metadata()?.len() < DESCRIPTOR_SET + 6 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(DESCRIPTOR_SET + 1))?;
    let mut id = [0u8; 5];
    file.read_exact(&mut id)?;
    let format = match &id {
        b"CD001" => Some("iso9660"),
        b"BEA01" => Some("udf"),
        _ => None,
    };
    tracing::debug!(target: "ternvale::config", path = %path.display(), format = ?format, "disc image probe");
    Ok(format)
}

#[cfg(test)]
#[path = "iso_tests.rs"]
mod tests;
