//! `[tpm]`: which backend answers the guest's TPM 2.0 commands.
//!
//! The CRB device exists in every VM; only ACPI guests can see it (the TPM2
//! table and `\_SB.TPM2` name it). `auto` (the default) forwards commands to
//! swtpm when it is installed and falls back to the error-answering stub with
//! a warning when it is not. `swtpm` makes a missing swtpm fatal, `stub`
//! never starts it. TPM state lives next to the UEFI variable store, see
//! [`VmConfig::tpm_state_path`].

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::{FirmwareTables, VmConfig};

/// Suffix appended to the NVRAM path for the swtpm state file.
pub const TPM_STATE_SUFFIX: &str = ".swtpm";

/// Who answers TPM commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TpmBackendKind {
    /// swtpm when installed, else the stub with a warning.
    #[default]
    Auto,
    /// swtpm, or the VM does not start.
    Swtpm,
    /// Every command gets `TPM_RC_COMMAND_CODE`; no swtpm process.
    Stub,
}

impl fmt::Display for TpmBackendKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Swtpm => "swtpm",
            Self::Stub => "stub",
        })
    }
}

/// The `[tpm]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TpmSection {
    /// `"auto"` (default), `"swtpm"` or `"stub"`.
    #[serde(default)]
    pub backend: TpmBackendKind,
    /// swtpm binary. Unset: `PATH`, then `/opt/homebrew/bin` and
    /// `/usr/local/bin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swtpm: Option<PathBuf>,
}

impl TpmSection {
    /// `swtpm` must be an existing file; `backend = "swtpm"` needs a guest
    /// that can see the TPM (`tables` is `acpi`).
    pub(crate) fn validate(&self, tables: FirmwareTables) -> Result<(), ConfigError> {
        if let Some(path) = &self.swtpm {
            if !path.is_file() {
                tracing::error!(target: "ternvale::config", path = %path.display(), "tpm.swtpm is not an existing file");
                return Err(ConfigError::MissingFile {
                    field: "tpm.swtpm".into(),
                    path: path.clone(),
                });
            }
        }
        if self.backend == TpmBackendKind::Swtpm && tables != FirmwareTables::Acpi {
            tracing::error!(target: "ternvale::config", firmware_tables = %tables, "tpm.backend = \"swtpm\" needs firmware_tables = \"acpi\"");
            return Err(ConfigError::TpmNeedsAcpi { tables });
        }
        tracing::debug!(target: "ternvale::config", backend = %self.backend, swtpm = ?self.swtpm, "accepted tpm section");
        Ok(())
    }
}

impl VmConfig {
    /// `[tpm]`, or its defaults when the table is absent.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn tpm_section(&self) -> TpmSection {
        self.tpm.clone().unwrap_or_default()
    }

    /// The swtpm state file: the NVRAM path with [`TPM_STATE_SUFFIX`]
    /// appended, so the TPM identity lives and moves with the UEFI variables
    /// of the same VM. `None` without `firmware` (no ACPI, no visible TPM).
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn tpm_state_path(&self) -> Result<Option<PathBuf>, ConfigError> {
        Ok(self.nvram_path()?.map(|nvram| {
            let mut name = nvram.into_os_string();
            name.push(TPM_STATE_SUFFIX);
            PathBuf::from(name)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_names_are_lowercase_and_auto_is_the_default() {
        assert_eq!(TpmBackendKind::default(), TpmBackendKind::Auto);
        let names: Vec<String> = [
            TpmBackendKind::Auto,
            TpmBackendKind::Swtpm,
            TpmBackendKind::Stub,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(names, ["auto", "swtpm", "stub"]);
        let section: TpmSection = toml::from_str("backend = \"stub\"").expect("parse");
        assert_eq!(section.backend, TpmBackendKind::Stub);
        assert!(toml::from_str::<TpmSection>("backend = \"cuse\"").is_err());
        assert!(toml::from_str::<TpmSection>("path = \"/x\"").is_err());
    }

    const BASE: &str = "name = \"w\"\ncpus = 1\nram_mib = 256\nserial_log = \"/tmp/s.log\"\n";

    #[test]
    fn state_file_sits_next_to_the_nvram_file() {
        let text = format!("{BASE}firmware = \"/fw.fd\"\nnvram = \"/vms/w/nvram.fd\"\n");
        let config = VmConfig::parse(&text).expect("parse");
        assert_eq!(
            config.tpm_state_path().expect("path"),
            Some(PathBuf::from("/vms/w/nvram.fd.swtpm"))
        );
        let direct = VmConfig::parse(&format!("{BASE}kernel = \"/k\"\n")).expect("parse");
        assert_eq!(direct.tpm_state_path().expect("path"), None);
    }

    #[test]
    fn tpm_table_parses_and_round_trips_through_vm_config() {
        let text = format!(
            "{BASE}firmware = \"/fw.fd\"\n[tpm]\nbackend = \"swtpm\"\nswtpm = \"/opt/swtpm\"\n"
        );
        let config = VmConfig::parse(&text).expect("parse");
        let section = config.tpm_section();
        assert_eq!(section.backend, TpmBackendKind::Swtpm);
        assert_eq!(section.swtpm, Some(PathBuf::from("/opt/swtpm")));
        let again: VmConfig = toml::from_str(&toml::to_string(&config).expect("ser")).expect("de");
        assert_eq!(again, config);
        let none = VmConfig::parse(&format!("{BASE}kernel = \"/k\"\n")).expect("parse");
        assert_eq!(none.tpm_section(), TpmSection::default());
        assert!(!toml::to_string(&none).expect("ser").contains("tpm"));
    }

    #[test]
    fn swtpm_backend_needs_acpi_and_the_binary_must_exist() {
        let swtpm = TpmSection {
            backend: TpmBackendKind::Swtpm,
            swtpm: None,
        };
        swtpm.validate(FirmwareTables::Acpi).expect("acpi");
        let error = swtpm.validate(FirmwareTables::Fdt).expect_err("fdt");
        assert!(
            error.to_string().contains("firmware_tables = \"acpi\""),
            "{error}"
        );
        TpmSection::default()
            .validate(FirmwareTables::Fdt)
            .expect("auto is fine without acpi");
        let missing = TpmSection {
            backend: TpmBackendKind::Auto,
            swtpm: Some(PathBuf::from("/no/such/swtpm")),
        };
        assert!(matches!(
            missing.validate(FirmwareTables::Acpi),
            Err(ConfigError::MissingFile { field, .. }) if field == "tpm.swtpm"
        ));
    }
}
