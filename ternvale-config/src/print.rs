//! `[print]` per-VM host printer sharing (opt-in).

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// Longest CUPS / display printer name.
pub const MAX_PRINTER_NAME: usize = 128;
/// Most host printers shared with one VM.
pub const MAX_SHARED_PRINTERS: usize = 32;

/// Per-VM printing. Omitted: off (no guest printers, no IPP proxy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrintSection {
    /// Share selected host printers with the guest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enabled: bool,
    /// Offer "Print to PDF on Mac" (opens Preview). Default true when the table exists.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub pdf: bool,
    /// Host CUPS destination names to share (exact `lpstat` names). Empty: none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub share: Vec<String>,
}

impl Default for PrintSection {
    fn default() -> Self {
        Self {
            enabled: false,
            pdf: true,
            share: Vec::new(),
        }
    }
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

impl PrintSection {
    /// True when this is all defaults (omitted from TOML).
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn is_default(&self) -> bool {
        !self.enabled && self.pdf && self.share.is_empty()
    }

    /// Range checks on shared names.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.share.len() > MAX_SHARED_PRINTERS {
            return Err(ConfigError::InvalidPrint {
                field: "print.share",
                reason: format!("at most {MAX_SHARED_PRINTERS} printers"),
            });
        }
        for (index, name) in self.share.iter().enumerate() {
            if name.is_empty() || name.len() > MAX_PRINTER_NAME {
                return Err(ConfigError::InvalidPrint {
                    field: "print.share",
                    reason: format!("[{index}] name length must be 1..={MAX_PRINTER_NAME}"),
                });
            }
            if name.contains('\0')
                || name.contains('/')
                || name.contains('\\')
                || name.contains("..")
            {
                return Err(ConfigError::InvalidPrint {
                    field: "print.share",
                    reason: format!("[{index}] must not contain NUL, slashes, or '..'"),
                });
            }
            if self.share[..index].iter().any(|other| other == name) {
                return Err(ConfigError::InvalidPrint {
                    field: "print.share",
                    reason: format!("[{index}] duplicated name"),
                });
            }
        }
        tracing::debug!(
            target: "ternvale::config",
            enabled = self.enabled,
            pdf = self.pdf,
            share = self.share.len(),
            "accepted print section"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_opt_in_off() {
        let section = PrintSection::default();
        assert!(!section.enabled);
        assert!(section.pdf);
        assert!(section.is_default());
        section.validate().expect("defaults");
    }

    #[test]
    fn rejects_path_and_duplicates() {
        let section = PrintSection {
            enabled: true,
            share: vec!["../evil".into()],
            ..Default::default()
        };
        assert!(section.validate().is_err());
        let section = PrintSection {
            enabled: true,
            share: vec!["HP".into(), "HP".into()],
            ..Default::default()
        };
        assert!(section.validate().is_err());
    }

    #[test]
    fn parses_opt_in_table() {
        let section: PrintSection =
            toml::from_str("enabled = true\nshare = [\"Office\"]\n").expect("toml");
        assert!(section.enabled);
        assert!(section.pdf);
        assert_eq!(section.share, ["Office"]);
        section.validate().expect("ok");
    }
}
