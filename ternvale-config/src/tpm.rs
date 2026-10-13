//! `[tpm]`: whether the VM has a TPM 2.0, and which backend answers it.
//!
//! The CRB device exists in every VM unless `enabled = false`; only ACPI
//! guests can see it (the TPM2 table and `\_SB.TPM2` name it). `auto` (the
//! default) forwards commands to swtpm when it is installed and falls back to
//! the error-answering stub with a warning when it is not. `swtpm` makes a
//! missing swtpm fatal, `stub` never starts it. `enabled = false` removes the
//! device, its ACPI description and the swtpm process: for guests (typically
//! Linux) that do not use a TPM. TPM state lives next to the UEFI variable
//! store, see [`VmConfig::tpm_state_path`].

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TpmSection {
    /// `false`: no TPM at all (no CRB device, no TPM2 table or `\_SB.TPM2`,
    /// no swtpm process). Default `true`.
    #[serde(default = "enabled_default", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// `"auto"` (default), `"swtpm"` or `"stub"`.
    #[serde(default)]
    pub backend: TpmBackendKind,
    /// swtpm binary. Unset: `PATH`, then `/opt/homebrew/bin` and
    /// `/usr/local/bin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swtpm: Option<PathBuf>,
}

impl Default for TpmSection {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: TpmBackendKind::default(),
            swtpm: None,
        }
    }
}

fn enabled_default() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

impl TpmSection {
    /// `swtpm` must be an existing file; `backend = "swtpm"` needs a guest
    /// that can see the TPM (`tables` is `acpi`). A disabled TPM takes no
    /// `backend = "swtpm"` or `swtpm` path: one of the two settings is a
    /// mistake, and silently dropping the TPM would hide it.
    pub(crate) fn validate(&self, tables: FirmwareTables) -> Result<(), ConfigError> {
        if !self.enabled {
            let conflict = if self.backend == TpmBackendKind::Swtpm {
                Some("tpm.backend = \"swtpm\"")
            } else if self.swtpm.is_some() {
                Some("tpm.swtpm")
            } else {
                None
            };
            if let Some(field) = conflict {
                tracing::error!(target: "ternvale::config", field, "tpm.enabled = false conflicts with a tpm setting");
                return Err(ConfigError::TpmDisabledConflict { field });
            }
            tracing::debug!(target: "ternvale::config", "tpm disabled");
            return Ok(());
        }
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

    /// False when `[tpm] enabled = false`: the VM gets no TPM device.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all, fields(name = %self.name))]
    pub fn tpm_enabled(&self) -> bool {
        self.tpm.as_ref().is_none_or(|tpm| tpm.enabled)
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
    fn enabled_defaults_to_true_and_false_round_trips() {
        let fw = format!("{BASE}firmware = \"/fw.fd\"\nfirmware_tables = \"acpi\"\n");
        assert!(VmConfig::parse(&fw).expect("parse").tpm_enabled());
        let on = VmConfig::parse(&format!("{fw}[tpm]\nbackend = \"stub\"\n")).expect("parse");
        assert!(on.tpm_enabled());
        assert!(!toml::to_string(&on).expect("ser").contains("enabled"));
        let off = VmConfig::parse(&format!("{fw}[tpm]\nenabled = false\n")).expect("parse");
        assert!(!off.tpm_enabled());
        assert!(!off.tpm_section().enabled);
        let text = toml::to_string(&off).expect("ser");
        assert!(text.contains("enabled = false"), "{text}");
        let again: VmConfig = toml::from_str(&text).expect("de");
        assert_eq!(again, off);
        let fdt = VmConfig::parse(&format!("{BASE}kernel = \"/k\"\n[tpm]\nenabled = false\n"))
            .expect("a direct boot may disable the tpm too");
        assert!(!fdt.tpm_enabled());
    }

    #[test]
    fn a_disabled_tpm_rejects_settings_that_need_one() {
        let section = |text: &str| toml::from_str::<TpmSection>(text).expect("parse");
        let error = section("enabled = false\nbackend = \"swtpm\"")
            .validate(FirmwareTables::Acpi)
            .expect_err("swtpm");
        assert!(
            matches!(
                error,
                ConfigError::TpmDisabledConflict {
                    field: "tpm.backend = \"swtpm\""
                }
            ),
            "{error}"
        );
        let error = section("enabled = false\nswtpm = \"/x\"")
            .validate(FirmwareTables::Acpi)
            .expect_err("path");
        assert!(error.to_string().contains("tpm.swtpm"), "{error}");
        assert!(error.to_string().contains("enabled = false"), "{error}");
        for tables in [FirmwareTables::Acpi, FirmwareTables::Fdt] {
            section("enabled = false\nbackend = \"stub\"")
                .validate(tables)
                .expect("stub is harmless");
        }
    }

    #[test]
    fn swtpm_backend_needs_acpi_and_the_binary_must_exist() {
        let swtpm = TpmSection {
            backend: TpmBackendKind::Swtpm,
            swtpm: None,
            enabled: true,
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
            enabled: true,
        };
        assert!(matches!(
            missing.validate(FirmwareTables::Acpi),
            Err(ConfigError::MissingFile { field, .. }) if field == "tpm.swtpm"
        ));
    }
}
