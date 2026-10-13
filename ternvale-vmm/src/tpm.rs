//! TPM 2.0 CRB at [`crate::platform::TPM_CRB_BASE`] on the MMIO bus.
//!
//! The register model lives in `ternvale-tpm`; this module adapts it to
//! [`MmioDevice`] and picks the command backend from `[tpm]` (docs/TPM.md):
//! swtpm over its socket interface, with state in `<nvram>.swtpm`, or the
//! stub that answers every command with `TPM_RC_COMMAND_CODE`. Guests find
//! the device through the ACPI TPM2 table and `\_SB.TPM2`
//! (`firmware_tables = "acpi"`); the acpi-only `tpm@` DT node exists for
//! EDK2. A guest without ACPI never sees the TPM, so it always gets the stub
//! and no swtpm is started.

use ternvale_config::{FirmwareTables, TpmBackendKind, VmConfig};
use ternvale_tpm::{StubBackend, SwtpmBackend, TpmBackend, TpmCrb, CRB_WINDOW_SIZE};

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

/// The command backend `config` asks for. `auto` falls back to the stub when
/// swtpm is missing or too old; any other swtpm failure (bad state file,
/// lock held by another VM, start-up error) stops the VM rather than hand the
/// guest a TPM with a different identity.
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(vm = %config.name))]
pub(crate) fn backend_for(config: &VmConfig) -> Result<Box<dyn TpmBackend>, MachineError> {
    let section = config.tpm_section();
    let tables = config.effective_firmware_tables();
    let state = match (section.backend, tables, config.tpm_state_path()?) {
        (TpmBackendKind::Stub, _, _) => {
            tracing::info!(target: "ternvale::tpm", "tpm backend is the stub ([tpm] backend = \"stub\"); every command gets TPM_RC_COMMAND_CODE");
            return Ok(Box::new(StubBackend::new()));
        }
        (_, FirmwareTables::Acpi, Some(state)) => state,
        (backend, tables, _) => {
            tracing::debug!(target: "ternvale::tpm", backend = %backend, firmware_tables = %tables, "guest cannot see the tpm without acpi; stub backend, swtpm not started");
            return Ok(Box::new(StubBackend::new()));
        }
    };
    match SwtpmBackend::start(section.swtpm.as_deref(), &state) {
        Ok(backend) => Ok(Box::new(backend)),
        Err(error) if section.backend == TpmBackendKind::Auto && error.is_unavailable() => {
            tracing::warn!(
                target: "ternvale::tpm",
                error = %error,
                "swtpm unavailable; the guest TPM answers every command with an error (no measured boot, Windows 11 will not install). Install swtpm with `brew install swtpm`; `ternvale doctor` checks it"
            );
            Ok(Box::new(StubBackend::new()))
        }
        Err(error) => {
            tracing::error!(target: "ternvale::tpm", error = %error, state = %state.display(), "swtpm backend failed");
            Err(MachineError::Attach(format!("tpm swtpm backend: {error}")))
        }
    }
}

/// The CRB over `backend`, ready for [`crate::MmioBus::register`].
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(base = %format!("{TPM_CRB_BASE:#x}"), backend = backend.name()))]
pub(crate) fn device(backend: Box<dyn TpmBackend>) -> Result<BusDevice, MachineError> {
    let crb = TpmCrb::new(TPM_CRB_BASE, backend)
        .map_err(|error| MachineError::Attach(format!("tpm crb: {error}")))?;
    Ok((TPM_CRB_BASE, CRB_WINDOW_SIZE, Box::new(crb)))
}

/// Put the CRB, with the backend `config` asks for, on `bus` at
/// [`TPM_CRB_BASE`].
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(vm = %config.name))]
pub(crate) fn register(bus: &mut MmioBus, config: &VmConfig) -> Result<(), MachineError> {
    let (base, size, crb) = device(backend_for(config)?)?;
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

#[cfg(test)]
#[path = "tpm_backend_tests.rs"]
mod backend_tests;
