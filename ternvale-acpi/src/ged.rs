//! DSDT Generic Event Device (`ACPI0013`) and power button (`PNP0C0C`).
//!
//! ACPI 6.5 §5.6.5.2: HID `ACPI0013`, `_CRS` interrupt, `_EVT` invoked with
//! the event index when the interrupt fires. `_EVT` notifies `PWRB` 0x80
//! (ACPI 6.5 §4.8.2.2.1.2 Device Wake / power button). Hardware-reduced
//! platforms have no fixed FADT power-button register.

use crate::aml::{device, method, name, notify};
use crate::aml_data::{eisa_id, integer, string};
use crate::aml_resource::{extended_interrupt, memory32_fixed, resource_template};
use crate::config::{spi_gsiv, GedConfig};
use crate::AcpiError;

/// ACPI 6.5 Generic Event Device HID.
const GED_HID: &str = "ACPI0013";
/// Power button HID (PNP0C0C).
const PWRB_HID: &str = "PNP0C0C";
/// Present + enabled + show in UI (typical power-button `_STA`).
const PWRB_STA: u64 = 0x0b;
/// Notify code for a power-button press (ACPI 6.5 Table 5.16 / device-wake).
const POWER_BUTTON_NOTIFY: u64 = 0x80;

/// `Device (PWRB)` then `Device (GED0)` with `_EVT` → `Notify (PWRB, 0x80)`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(base = %format!("{:#x}+{:#x}", ged.mmio_base, ged.mmio_len), spi = ged.spi)
)]
pub fn power_devices(ged: &GedConfig) -> Result<Vec<u8>, AcpiError> {
    let pwrb = {
        let body = [
            name("_HID", &eisa_id(PWRB_HID)?)?,
            name("_UID", &integer(0))?,
            name("_STA", &integer(PWRB_STA))?,
        ]
        .concat();
        device("PWRB", &body)?
    };
    let gsiv = spi_gsiv(ged.spi);
    let crs = resource_template(&[
        memory32_fixed(ged.mmio_base, u64::from(ged.mmio_len))?,
        extended_interrupt(gsiv)?,
    ])?;
    let evt = method("_EVT", 1, &notify("PWRB", POWER_BUTTON_NOTIFY)?)?;
    let ged_body = [
        name("_HID", &string(GED_HID)?)?,
        name("_UID", &integer(0))?,
        name("_CRS", &crs)?,
        evt,
    ]
    .concat();
    let ged0 = device("GED0", &ged_body)?;
    tracing::debug!(
        target: "ternvale::acpi",
        gsiv,
        bytes = pwrb.len() + ged0.len(),
        "dsdt power button and GED built"
    );
    Ok([pwrb, ged0].concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::sample;

    #[test]
    fn ged_and_pwrb_are_named_devices() {
        let aml = power_devices(&sample(1).ged).expect("ged");
        assert!(aml.windows(4).any(|w| w == b"PWRB"));
        assert!(aml.windows(4).any(|w| w == b"GED0"));
        assert!(aml.windows(4).any(|w| w == b"_EVT"));
    }
}
