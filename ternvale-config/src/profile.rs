//! `profile`: a guest OS preset, `"linux"` (the default) or `"windows"`.
//!
//! `linux` changes nothing: every other field keeps its own default.
//! `windows` selects what Windows 11 on Arm needs from the VM:
//! - `firmware_tables = "acpi"`: Windows reads ACPI and never a device tree,
//!   so `firmware` is required and an explicit `"fdt"` is rejected;
//! - a TPM 2.0: `[tpm] enabled = false` is rejected;
//! - virtio-pci instead of virtio-mmio for every device that has a PCI
//!   transport ([`VmConfig::virtio_transport`]). Windows finds devices by PCI
//!   enumeration, and the virtio-win drivers are PCI drivers;
//! - a UEFI boot order that starts with the attached ISO
//!   ([`VmConfig::iso_boots_first`]; the VMM writes fw_cfg `bootorder`);
//! - `cpus` and `ram_mib` default to the Windows 11 minimums when omitted,
//!   and lower values are rejected.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::{FirmwareTables, TpmBackendKind, VmConfig};

// Windows 11 minimums. Source: Microsoft Learn, "Windows 11 requirements",
// section "Virtual machine support" (fetched 2026-10-13):
// https://learn.microsoft.com/en-us/windows/whats-new/windows-11-requirements
// It lists "Memory: 4 GB or greater", "Virtual processors: 2 or more",
// "Storage: 64 GB or greater" and "Secure Boot capable with virtual TPM
// enabled". https://www.microsoft.com/en-us/windows/windows-11-specifications
// gives the same 4 GB RAM, 2 or more cores, 64 GB storage and TPM 2.0.
// TODO(verify): "GB" is read as GiB (4096 MiB also exceeds 4 * 10^9 bytes);
// recheck both pages when Microsoft revises them, and whether Windows 11 on
// Arm states different numbers than the general list.

/// Fewest vCPUs Windows 11 accepts ("2 or more" virtual processors).
pub const WINDOWS_MIN_CPUS: u32 = 2;
/// Least guest RAM Windows 11 accepts, in MiB ("4 GB or greater").
pub const WINDOWS_MIN_RAM_MIB: u64 = 4096;
/// Smallest system disk Windows 11 installs to ("64 GB or greater"). Only
/// warned about: the config cannot tell which disk Windows will use.
pub const WINDOWS_MIN_DISK_BYTES: u64 = 64 << 30;

/// Guest OS preset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// No preset; today's defaults.
    #[default]
    Linux,
    /// ACPI, TPM 2.0, virtio-pci, ISO first in the boot order, and the
    /// Windows 11 RAM and vCPU minimums.
    Windows,
}

impl fmt::Display for Profile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
        })
    }
}

/// How virtio devices reach the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioTransport {
    /// virtio-mmio slots described in the device tree.
    Mmio,
    /// Functions behind the PCIe host bridge.
    Pci,
}

impl fmt::Display for VirtioTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Mmio => "virtio-mmio",
            Self::Pci => "virtio-pci",
        })
    }
}

impl Profile {
    /// For `skip_serializing_if`: `linux` is the default and is not written.
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all, fields(profile = %self))]
    pub fn is_linux(&self) -> bool {
        *self == Self::Linux
    }

    /// `cpus` when the document omits it. `None`: the field is required.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(profile = %self))]
    pub fn default_cpus(self) -> Option<u32> {
        match self {
            Self::Linux => None,
            Self::Windows => Some(WINDOWS_MIN_CPUS),
        }
    }

    /// `ram_mib` when the document omits it. `None`: the field is required.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(profile = %self))]
    pub fn default_ram_mib(self) -> Option<u64> {
        match self {
            Self::Linux => None,
            Self::Windows => Some(WINDOWS_MIN_RAM_MIB),
        }
    }

    /// Reject settings the profile cannot run with.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(profile = %self, name = %config.name))]
    pub(crate) fn validate(self, config: &VmConfig) -> Result<(), ConfigError> {
        if self == Self::Linux {
            tracing::debug!(target: "ternvale::config", "profile linux; no preset");
            return Ok(());
        }
        if config.firmware.is_none() {
            tracing::error!(target: "ternvale::config", profile = %self, "profile needs firmware");
            return Err(ConfigError::ProfileNeedsFirmware { profile: self });
        }
        if config.firmware_tables == Some(FirmwareTables::Fdt) {
            return Err(conflict(
                self,
                "firmware_tables = \"fdt\"",
                "Windows reads ACPI tables, not a device tree",
            ));
        }
        if !config.tpm_enabled() {
            return Err(conflict(
                self,
                "tpm.enabled = false",
                "Windows 11 requires a TPM 2.0",
            ));
        }
        at_least(
            self,
            "cpus",
            u64::from(config.cpus),
            u64::from(WINDOWS_MIN_CPUS),
            "Windows 11 requires at least 2 virtual processors",
        )?;
        at_least(
            self,
            "ram_mib",
            config.ram_mib,
            WINDOWS_MIN_RAM_MIB,
            "Windows 11 requires at least 4 GB of memory",
        )?;
        if config.tpm_section().backend == TpmBackendKind::Stub {
            tracing::warn!(target: "ternvale::config", profile = %self, "tpm.backend = \"stub\" fails every TPM command; Windows 11 setup will report that the PC lacks a TPM 2.0");
        }
        warn_small_disks(self, config);
        warn_missing_drivers(self, config);
        tracing::debug!(
            target: "ternvale::config",
            profile = %self,
            cpus = config.cpus,
            ram_mib = config.ram_mib,
            transport = %config.virtio_transport(),
            "windows profile: acpi tables, tpm 2.0, virtio-pci disks, iso first in the uefi boot order"
        );
        Ok(())
    }
}

fn conflict(profile: Profile, field: &'static str, reason: &'static str) -> ConfigError {
    tracing::error!(target: "ternvale::config", %profile, field, reason, "setting conflicts with the profile");
    ConfigError::ProfileConflict {
        profile,
        field,
        reason,
    }
}

fn at_least(
    profile: Profile,
    field: &'static str,
    got: u64,
    minimum: u64,
    requirement: &'static str,
) -> Result<(), ConfigError> {
    if got >= minimum {
        tracing::debug!(target: "ternvale::config", %profile, field, got, minimum, "meets the profile minimum");
        return Ok(());
    }
    tracing::error!(target: "ternvale::config", %profile, field, got, minimum, requirement, "below the profile minimum");
    Err(ConfigError::BelowProfileMinimum {
        profile,
        field,
        minimum,
        got,
        requirement,
    })
}

/// Warn when no writable disk could hold Windows. Sizes are apparent sizes,
/// so a sparse image counts at its full length.
fn warn_small_disks(profile: Profile, config: &VmConfig) {
    let largest = config
        .disks
        .iter()
        .filter(|disk| !disk.read_only)
        .filter_map(|disk| std::fs::metadata(&disk.path).ok().map(|meta| meta.len()))
        .max();
    match largest {
        Some(bytes) if bytes >= WINDOWS_MIN_DISK_BYTES => {
            tracing::debug!(target: "ternvale::config", %profile, bytes, "writable disk is large enough for Windows 11");
        }
        largest => tracing::warn!(
            target: "ternvale::config",
            %profile,
            largest_writable_bytes = largest.unwrap_or(0),
            minimum_bytes = WINDOWS_MIN_DISK_BYTES,
            "no writable disk of at least 64 GiB; Windows 11 setup needs one to install to"
        ),
    }
}

/// Windows has no inbox virtio driver, so Setup lists no disk until it loads
/// `viostor` from a driver disc.
fn warn_missing_drivers(profile: Profile, config: &VmConfig) {
    let iso = config.iso.as_ref();
    if iso.is_some_and(|iso| iso.installer.is_some() && iso.drivers.is_none()) {
        tracing::warn!(
            target: "ternvale::config",
            %profile,
            "iso.installer without iso.drivers: Windows Setup has no virtio driver and will list no disk; attach virtio-win (scripts/fetch-virtio-win.sh)"
        );
    }
}

impl VmConfig {
    /// `virtio-pci` for the `windows` profile, else `virtio-mmio`. Devices
    /// without a PCI transport stay on virtio-mmio either way.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name, profile = %self.profile))]
    pub fn virtio_transport(&self) -> VirtioTransport {
        match self.profile {
            Profile::Linux => VirtioTransport::Mmio,
            Profile::Windows => VirtioTransport::Pci,
        }
    }

    /// True when the UEFI boot order must start with the attached ISO images.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name, profile = %self.profile))]
    pub fn iso_boots_first(&self) -> bool {
        self.profile == Profile::Windows
    }
}

/// Parse a VM config document, filling `cpus` and `ram_mib` from the
/// profile when the document omits them. Documents that need no filling go
/// through `toml::from_str` so errors keep their line and column.
pub(crate) fn parse(text: &str) -> Result<VmConfig, toml::de::Error> {
    let mut table: toml::Table = text.parse()?;
    let profile = table
        .get("profile")
        .and_then(|value| Profile::deserialize(value.clone()).ok())
        .unwrap_or_default();
    let mut filled = false;
    if let Some(cpus) = profile
        .default_cpus()
        .filter(|_| !table.contains_key("cpus"))
    {
        table.insert("cpus".into(), toml::Value::Integer(i64::from(cpus)));
        tracing::debug!(target: "ternvale::config", %profile, cpus, "cpus omitted; profile default applied");
        filled = true;
    }
    if let Some(ram_mib) = profile
        .default_ram_mib()
        .filter(|_| !table.contains_key("ram_mib"))
    {
        let ram = i64::try_from(ram_mib).unwrap_or(i64::MAX);
        table.insert("ram_mib".into(), toml::Value::Integer(ram));
        tracing::debug!(target: "ternvale::config", %profile, ram_mib, "ram_mib omitted; profile default applied");
        filled = true;
    }
    if filled {
        toml::Value::Table(table).try_into()
    } else {
        toml::from_str(text)
    }
}

/// True when `path` holds an ISO 9660 image: the volume descriptor set starts
/// at logical sector 16 (2048-byte sectors; sectors 0 to 15 are the System
/// Area), and every descriptor carries the standard identifier `CD001` in
/// bytes 1 to 5 (ECMA-119 4th edition, 6.2.1 and 8.1). Windows install ISOs
/// are UDF bridge images and carry it as well.
/// TODO(verify): check a Windows 11 Arm64 ISO from Microsoft.
#[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(path = %path.display()))]
pub fn is_iso9660(path: &Path) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    const DESCRIPTOR_SET: u64 = 16 * 2048;
    let mut file = std::fs::File::open(path)?;
    if file.metadata()?.len() < DESCRIPTOR_SET + 6 {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(DESCRIPTOR_SET + 1))?;
    let mut id = [0u8; 5];
    file.read_exact(&mut id)?;
    let iso = &id == b"CD001";
    tracing::debug!(target: "ternvale::config", path = %path.display(), iso, "iso 9660 probe");
    Ok(iso)
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
