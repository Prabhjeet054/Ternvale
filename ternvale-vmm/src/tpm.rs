//! TPM 2.0 CRB at [`crate::platform::TPM_CRB_BASE`] on the MMIO bus.
//!
//! The register model lives in `ternvale-tpm`; this module only adapts it to
//! [`MmioDevice`]. Commands complete synchronously inside the `START` write and
//! get stub responses (`ternvale_tpm::StubBackend`). Guests find the device
//! through the ACPI TPM2 table and `\_SB.TPM2` (`firmware_tables = "acpi"`);
//! there is no device tree node, so a DT-only guest never probes it.

use ternvale_tpm::{StubBackend, TpmCrb, CRB_WINDOW_SIZE};

use crate::acpi::BusDevice;
use crate::machine::MachineError;
use crate::mmio::{MmioBus, MmioDevice};
use crate::platform::{TPM_CRB_BASE, TPM_CRB_SIZE};

/// Name in MMIO logs and the shutdown device counters.
pub const TPM_DEVICE_NAME: &str = "tpm-crb";

const _: () = assert!(CRB_WINDOW_SIZE <= TPM_CRB_SIZE);

impl MmioDevice for TpmCrb {
    fn name(&self) -> &str {
        TPM_DEVICE_NAME
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        TpmCrb::read(self, offset, size)
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        TpmCrb::write(self, offset, size, val);
    }
}

/// The CRB with a stub command backend, ready for [`crate::MmioBus::register`].
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(base = %format!("{TPM_CRB_BASE:#x}")))]
pub(crate) fn device() -> Result<BusDevice, MachineError> {
    let crb = TpmCrb::new(TPM_CRB_BASE, Box::new(StubBackend::new()))
        .map_err(|error| MachineError::Attach(format!("tpm crb: {error}")))?;
    Ok((TPM_CRB_BASE, CRB_WINDOW_SIZE, Box::new(crb)))
}

/// Put the CRB on `bus` at [`TPM_CRB_BASE`].
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
pub(crate) fn register(bus: &mut MmioBus) -> Result<(), MachineError> {
    let (base, size, crb) = device()?;
    bus.register(base, size, crb)?;
    tracing::debug!(
        target: "ternvale::tpm",
        base = %format!("{base:#x}"),
        size = %format!("{size:#x}"),
        "tpm crb registered on the mmio bus"
    );
    Ok(())
}

#[cfg(test)]
#[path = "tpm_tests.rs"]
mod tests;
