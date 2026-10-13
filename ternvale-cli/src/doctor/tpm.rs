//! swtpm, the optional runtime dependency behind the guest TPM (docs/TPM.md).
//!
//! Without it `[tpm] backend = "auto"` (the default) falls back to a stub
//! that answers every TPM command with an error, so a missing swtpm warns;
//! a VM config with `backend = "swtpm"` turns that into a failure, and a
//! config that never uses swtpm (stub, no ACPI, or `enabled = false`) skips
//! the check. A swtpm that has every required feature but is older than
//! [`TESTED_SWTPM_VERSION`], or whose version cannot be read, warns.

use std::path::Path;

use ternvale_config::{FirmwareTables, TpmBackendKind, VmConfig};
use ternvale_tpm::{
    find_swtpm, parse_swtpm_version, SwtpmError, SwtpmInfo, REQUIRED_FEATURES, TESTED_SWTPM_VERSION,
};

use super::{Check, Outcome};

/// Report name of the check.
pub const NAME: &str = "swtpm";

/// Whether a VM needs swtpm, from its `[tpm]` and `firmware_tables`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// `auto`: used when present, the stub otherwise.
    Optional,
    /// `backend = "swtpm"`: the VM does not start without it.
    Required,
    /// The stub is configured, or the guest cannot see the TPM.
    Unused(&'static str),
}

/// What the VM config at `path` needs, and the swtpm it would use. `None`
/// when the config cannot be read (the vm config check reports that).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn need_for(path: &Path) -> Option<(Need, Result<SwtpmInfo, SwtpmError>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let config = VmConfig::parse(&text).ok()?;
    let section = config.tpm_section();
    let need = match (section.backend, config.effective_firmware_tables()) {
        _ if !section.enabled => Need::Unused("[tpm] enabled = false, the VM has no TPM"),
        (TpmBackendKind::Stub, _) => Need::Unused("[tpm] backend = \"stub\""),
        (_, FirmwareTables::Fdt) => Need::Unused("the guest has no ACPI, so it cannot see the TPM"),
        (TpmBackendKind::Swtpm, _) => Need::Required,
        (TpmBackendKind::Auto, _) => Need::Optional,
    };
    tracing::debug!(target: "ternvale::cli", need = ?need, swtpm = ?section.swtpm, "tpm backend need");
    Some((need, find_swtpm(section.swtpm.as_deref())))
}

/// The swtpm check: PASS with its version, otherwise how to install it.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(need = ?need))]
pub fn check_swtpm(found: &Result<SwtpmInfo, SwtpmError>, need: Need) -> Check {
    let missing = match need {
        Need::Required => Outcome::Fail,
        _ => Outcome::Warn,
    };
    let (major, minor) = TESTED_SWTPM_VERSION;
    match (found, need) {
        (_, Need::Unused(why)) => Check::new(NAME, Outcome::Skip, format!("not used: {why}")),
        (Ok(info), _) => match parse_swtpm_version(&info.version) {
            Some((a, b, _)) if (a, b) >= TESTED_SWTPM_VERSION => Check::new(
                NAME,
                Outcome::Pass,
                format!("swtpm {} at {}", info.version, info.path.display()),
            ),
            parsed => Check::new(
                NAME,
                Outcome::Warn,
                format!(
                    "swtpm {} at {} has every feature Ternvale needs, but {}; Ternvale is tested with {major}.{minor} or newer",
                    info.version,
                    info.path.display(),
                    if parsed.is_some() { "is older" } else { "its version cannot be read" }
                ),
            )
            .fix(format!("Upgrade it: `brew upgrade swtpm` (or point `[tpm] swtpm` at swtpm {major}.{minor} or newer).")),
        },
        (Err(SwtpmError::NotFound { searched }), _) => Check::new(
            NAME,
            missing,
            format!(
                "not found (looked for {searched}); without it the guest TPM answers every command with an error, so measured boot and Windows 11 setup fail"
            ),
        )
        .fix("Install it: `brew install swtpm`, or set `[tpm] swtpm = \"/path/to/swtpm\"` in the VM config. See docs/TPM.md."),
        (Err(error @ (SwtpmError::Unsupported { .. } | SwtpmError::Probe { .. })), _) => {
            Check::new(NAME, missing, error.to_string()).fix(format!(
                "Upgrade it: `brew upgrade swtpm`. `swtpm socket --print-capabilities` must list {}; Ternvale is tested with swtpm {major}.{minor} or newer.",
                REQUIRED_FEATURES.join(", ")
            ))
        }
        (Err(error), _) => Check::new(NAME, missing, error.to_string())
            .fix("Run `swtpm socket --print-capabilities` by hand; see docs/TPM.md."),
    }
}

#[cfg(test)]
#[path = "tpm_tests.rs"]
mod tests;
