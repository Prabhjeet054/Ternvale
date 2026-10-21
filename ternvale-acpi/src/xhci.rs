//! DSDT `\_SB.XHC0` for the platform xHCI MMIO device.
//!
//! Linux `xhci-acpi` matches `_HID` `PNP0D10` (USB xHCI compliant host).

use crate::aml::{device, name};
use crate::aml_data::{integer, string};
use crate::aml_resource::{extended_interrupt, memory32_fixed, resource_template};
use crate::config::{spi_gsiv, UsbConfig};
use crate::AcpiError;

/// ACPI _HID for xHCI (PNP0D10).
const XHCI_HID: &str = "PNP0D10";

/// `Device (XHC0) { _HID, _UID, _CRS }`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(base = %format!("{:#x}+{:#x}", usb.mmio_base, usb.mmio_len))
)]
pub fn xhci_device(usb: &UsbConfig) -> Result<Vec<u8>, AcpiError> {
    let gsiv = spi_gsiv(usb.spi);
    let crs = resource_template(&[
        memory32_fixed(usb.mmio_base, u64::from(usb.mmio_len))?,
        extended_interrupt(gsiv)?,
    ])?;
    let body = [
        name("_HID", &string(XHCI_HID)?)?,
        name("_UID", &integer(0))?,
        name("_CRS", &crs)?,
    ]
    .concat();
    let aml = device("XHC0", &body)?;
    tracing::debug!(target: "ternvale::acpi", bytes = aml.len(), gsiv, "dsdt xhci device built");
    Ok(aml)
}
