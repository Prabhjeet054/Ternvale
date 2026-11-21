//! DSDT `\_SB.PANC`: QEMU pvpanic ACPI device.
//!
//! Linux `drivers/misc/pvpanic/pvpanic-acpi.c` matches `_HID "QEMU0001"`.
//! `_CRS` is one `Memory32Fixed` over the 2-byte event register.

use crate::aml::{device, name};
use crate::aml_data::{integer, string};
use crate::aml_resource::{memory32_fixed, resource_template};
use crate::config::PvpanicConfig;
use crate::AcpiError;

/// ACPI HID (Linux `pvpanic-acpi`).
pub const PVPANIC_HID: &str = "QEMU0001";

/// `Device (PANC)` under `\_SB`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(base = %format!("{:#x}", pv.mmio_base))
)]
pub fn pvpanic_device(pv: &PvpanicConfig) -> Result<Vec<u8>, AcpiError> {
    let crs = resource_template(&[memory32_fixed(pv.mmio_base, u64::from(pv.mmio_len))?])?;
    let body = [
        name("_HID", &string(PVPANIC_HID)?)?,
        name("_UID", &integer(0))?,
        name("_CRS", &crs)?,
    ]
    .concat();
    let aml = device("PANC", &body)?;
    tracing::debug!(target: "ternvale::acpi", bytes = aml.len(), "dsdt pvpanic device built");
    Ok(aml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::sample;

    #[test]
    fn device_has_qemu0001_and_the_mmio_window() {
        let aml = pvpanic_device(&sample(1).pvpanic.expect("pv")).expect("device");
        let hid = string("QEMU0001").expect("string");
        assert!(aml.windows(hid.len()).any(|w| w == hid.as_slice()));
        assert!(aml.windows(4).any(|w| w == b"PANC"));
        let window = memory32_fixed(0x0905_0000, 2).expect("crs");
        assert!(aml.windows(window.len()).any(|w| w == window.as_slice()));
    }
}
