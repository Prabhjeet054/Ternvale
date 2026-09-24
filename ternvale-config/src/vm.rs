//! Ternvale's own VM config document.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

const RAM_QUANTUM_MIB: u64 = 16;
const MIN_CPUS: u32 = 1;
const MAX_CPUS: u32 = 16;

/// One virtual disk. `path` must be an existing file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disk {
    /// Host path of the disk image.
    pub path: PathBuf,
    /// When true, the guest sees a read-only disk.
    pub read_only: bool,
}

/// One virtual NIC. Only `backend` is defined until the net device lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nic {
    /// Host backend name. Interpretation is left to the net device.
    pub backend: String,
}

/// VM description loaded from TOML.
///
/// `from_toml` and `from_path` validate before returning. A value built in code
/// should be passed through [`VmConfig::validate`] before use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmConfig {
    /// Guest name. Also used in the host log file name.
    pub name: String,
    /// vCPU count, from 1 through 16.
    pub cpus: u32,
    /// Guest RAM in MiB. Must be a positive multiple of 16.
    pub ram_mib: u64,
    /// Linux kernel (or other boot image) on the host.
    pub kernel: PathBuf,
    /// Optional initrd. Omitted when the guest boots from the kernel alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initrd: Option<PathBuf>,
    /// Kernel command line. May be empty.
    #[serde(default)]
    pub cmdline: String,
    /// Disk images, in guest attachment order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<Disk>,
    /// NICs, in guest attachment order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nics: Vec<Nic>,
    /// Host file that will receive guest serial output. Created later; it need not exist yet.
    pub serial_log: PathBuf,
    /// Optional firmware image, such as UEFI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<PathBuf>,
}

impl VmConfig {
    /// Parse `text` and validate it.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let config = toml::from_str::<Self>(text).map_err(|source| {
            tracing::error!(
                target: "ternvale::config",
                error = %source,
                "failed to parse VM config"
            );
            ConfigError::Parse { source }
        })?;
        config.validate()?;
        tracing::debug!(
            target: "ternvale::config",
            name = %config.name,
            cpus = config.cpus,
            ram_mib = config.ram_mib,
            "parsed VM config"
        );
        Ok(config)
    }

    /// Read a TOML file and validate it.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::config",
        skip_all,
        fields(path = %path.display())
    )]
    pub fn from_path(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| {
            tracing::error!(
                target: "ternvale::config",
                path = %path.display(),
                error = %source,
                "failed to read VM config"
            );
            ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }
        })?;
        Self::from_toml(&text)
    }

    /// Serialize a validated config to TOML.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::config",
        skip_all,
        fields(name = %self.name)
    )]
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        self.validate()?;
        toml::to_string_pretty(self).map_err(|source| {
            tracing::error!(
                target: "ternvale::config",
                name = %self.name,
                error = %source,
                "failed to serialize VM config"
            );
            ConfigError::Serialize { source }
        })
    }

    /// Check ranges and that input files exist.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::config",
        skip_all,
        fields(name = %self.name, cpus = self.cpus, ram_mib = self.ram_mib)
    )]
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_name(&self.name)?;
        validate_cpus(self.cpus)?;
        validate_ram(self.ram_mib)?;
        require_file("kernel", &self.kernel)?;
        if let Some(initrd) = &self.initrd {
            require_file("initrd", initrd)?;
        } else {
            tracing::debug!(target: "ternvale::config", "initrd omitted");
        }
        for (index, disk) in self.disks.iter().enumerate() {
            require_file(&format!("disks[{index}].path"), &disk.path)?;
            tracing::debug!(
                target: "ternvale::config",
                index,
                path = %disk.path.display(),
                read_only = disk.read_only,
                "accepted disk"
            );
        }
        for (index, nic) in self.nics.iter().enumerate() {
            if nic.backend.is_empty() {
                tracing::error!(
                    target: "ternvale::config",
                    index,
                    "rejected NIC with empty backend"
                );
                return Err(ConfigError::EmptyNicBackend { index });
            }
            tracing::debug!(
                target: "ternvale::config",
                index,
                backend = %nic.backend,
                "accepted NIC"
            );
        }
        validate_serial_log(&self.serial_log)?;
        if let Some(firmware) = &self.firmware {
            require_file("firmware", firmware)?;
        } else {
            tracing::debug!(target: "ternvale::config", "firmware omitted");
        }
        tracing::debug!(target: "ternvale::config", name = %self.name, "VM config is valid");
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<(), ConfigError> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if ok {
        tracing::debug!(target: "ternvale::config", name, "accepted VM name");
        Ok(())
    } else {
        tracing::error!(target: "ternvale::config", name, "rejected VM name");
        Err(ConfigError::InvalidName)
    }
}

fn validate_cpus(cpus: u32) -> Result<(), ConfigError> {
    if (MIN_CPUS..=MAX_CPUS).contains(&cpus) {
        tracing::debug!(target: "ternvale::config", cpus, "accepted cpu count");
        Ok(())
    } else {
        tracing::error!(target: "ternvale::config", cpus, "rejected cpu count");
        Err(ConfigError::InvalidCpus { cpus })
    }
}

fn validate_ram(ram_mib: u64) -> Result<(), ConfigError> {
    if ram_mib > 0 && ram_mib % RAM_QUANTUM_MIB == 0 {
        tracing::debug!(target: "ternvale::config", ram_mib, "accepted ram_mib");
        Ok(())
    } else {
        tracing::error!(target: "ternvale::config", ram_mib, "rejected ram_mib");
        Err(ConfigError::InvalidRam { ram_mib })
    }
}

fn require_file(field: &str, path: &Path) -> Result<(), ConfigError> {
    if path.is_file() {
        tracing::debug!(
            target: "ternvale::config",
            field,
            path = %path.display(),
            "accepted file"
        );
        Ok(())
    } else {
        tracing::error!(
            target: "ternvale::config",
            field,
            path = %path.display(),
            "required file is missing"
        );
        Err(ConfigError::MissingFile {
            field: field.to_string(),
            path: path.to_path_buf(),
        })
    }
}

fn validate_serial_log(path: &Path) -> Result<(), ConfigError> {
    if path.as_os_str().is_empty() || path.is_dir() {
        tracing::error!(
            target: "ternvale::config",
            path = %path.display(),
            "rejected serial_log"
        );
        return Err(ConfigError::SerialLogDir {
            path: path.to_path_buf(),
        });
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            tracing::error!(
                target: "ternvale::config",
                path = %path.display(),
                parent = %parent.display(),
                "serial_log parent directory is missing"
            );
            return Err(ConfigError::SerialLogDir {
                path: path.to_path_buf(),
            });
        }
    }
    tracing::debug!(
        target: "ternvale::config",
        path = %path.display(),
        "accepted serial_log"
    );
    Ok(())
}
