//! `firmware_tables`: which hardware description the guest OS receives.
//!
//! `fdt` hands the OS the device tree (the Linux path). `acpi` also builds the
//! ACPI tables and offers them to EDK2 through QEMU's fw_cfg table loader;
//! EDK2 then installs ACPI and withholds the device tree from the OS (the
//! Windows path). Direct kernel boots have no firmware to install ACPI, so
//! `acpi` requires `firmware`. Unset means `fdt` without firmware and `acpi`
//! with it.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

/// Hardware description handed to the guest OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FirmwareTables {
    /// ACPI tables, installed by EDK2 from fw_cfg. Needs `firmware`.
    Acpi,
    /// The device tree only.
    Fdt,
}

impl fmt::Display for FirmwareTables {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Acpi => "acpi",
            Self::Fdt => "fdt",
        })
    }
}

/// `explicit` if set, else `acpi` for a firmware boot and `fdt` otherwise.
pub(crate) fn resolve(explicit: Option<FirmwareTables>, firmware: bool) -> FirmwareTables {
    match explicit {
        Some(tables) => tables,
        None if firmware => FirmwareTables::Acpi,
        None => FirmwareTables::Fdt,
    }
}

/// Reject `acpi` without `firmware`.
pub(crate) fn validate(
    explicit: Option<FirmwareTables>,
    firmware: bool,
) -> Result<(), ConfigError> {
    let tables = resolve(explicit, firmware);
    if tables == FirmwareTables::Acpi && !firmware {
        tracing::error!(
            target: "ternvale::config",
            "firmware_tables = \"acpi\" needs firmware; a direct kernel boot has no UEFI to install ACPI"
        );
        return Err(ConfigError::AcpiWithoutFirmware);
    }
    tracing::debug!(
        target: "ternvale::config",
        firmware_tables = %tables,
        explicit = explicit.is_some(),
        "accepted firmware_tables"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_follows_the_boot_path() {
        assert_eq!(resolve(None, false), FirmwareTables::Fdt);
        assert_eq!(resolve(None, true), FirmwareTables::Acpi);
        assert_eq!(
            resolve(Some(FirmwareTables::Fdt), true),
            FirmwareTables::Fdt
        );
        assert_eq!(
            resolve(Some(FirmwareTables::Acpi), true),
            FirmwareTables::Acpi
        );
    }

    #[test]
    fn acpi_needs_firmware() {
        assert!(matches!(
            validate(Some(FirmwareTables::Acpi), false),
            Err(ConfigError::AcpiWithoutFirmware)
        ));
        validate(Some(FirmwareTables::Acpi), true).expect("acpi with firmware");
        validate(Some(FirmwareTables::Fdt), false).expect("fdt without firmware");
        validate(None, false).expect("default without firmware");
        validate(None, true).expect("default with firmware");
    }

    #[test]
    fn names_are_lowercase() {
        assert_eq!(FirmwareTables::Acpi.to_string(), "acpi");
        assert_eq!(FirmwareTables::Fdt.to_string(), "fdt");
    }
}
