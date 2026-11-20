//! `[clipboard]` rich pasteboard and drag-and-drop policy.

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// Default size that requires a confirmation prompt, in MiB.
pub const DEFAULT_CONFIRM_MIB: u64 = 32;
/// Default transfer cap, in MiB.
pub const DEFAULT_MAX_MIB: u64 = 256;
/// Default file count cap.
pub const DEFAULT_MAX_FILES: u32 = 4096;

/// Per-VM clipboard / DnD. Omitted: every flag off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipboardSection {
    /// PNG/DIB/RTF/HTML/file-list clipboard (not only plain text).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rich: bool,
    /// Finder drop into the VM view and guest drag-out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dnd: bool,
    /// Prompt before transferring more than this many MiB.
    #[serde(
        default = "default_confirm_mib",
        skip_serializing_if = "is_default_confirm"
    )]
    pub confirm_mib: u64,
    /// Hard cap on a transfer, in MiB.
    #[serde(default = "default_max_mib", skip_serializing_if = "is_default_max")]
    pub max_mib: u64,
    /// Guest drop folder: `desktop`, `downloads`, or `documents`.
    #[serde(
        default = "default_guest_drop",
        skip_serializing_if = "is_default_drop"
    )]
    pub guest_drop: String,
    /// Max files in one clipboard list or drag.
    #[serde(
        default = "default_max_files",
        skip_serializing_if = "is_default_files"
    )]
    pub max_files: u32,
}

impl Default for ClipboardSection {
    fn default() -> Self {
        Self {
            rich: false,
            dnd: false,
            confirm_mib: DEFAULT_CONFIRM_MIB,
            max_mib: DEFAULT_MAX_MIB,
            guest_drop: default_guest_drop(),
            max_files: DEFAULT_MAX_FILES,
        }
    }
}

fn default_confirm_mib() -> u64 {
    DEFAULT_CONFIRM_MIB
}
fn default_max_mib() -> u64 {
    DEFAULT_MAX_MIB
}
fn default_guest_drop() -> String {
    "desktop".into()
}
fn default_max_files() -> u32 {
    DEFAULT_MAX_FILES
}
fn is_default_confirm(v: &u64) -> bool {
    *v == DEFAULT_CONFIRM_MIB
}
fn is_default_max(v: &u64) -> bool {
    *v == DEFAULT_MAX_MIB
}
fn is_default_drop(v: &str) -> bool {
    v == "desktop"
}
fn is_default_files(v: &u32) -> bool {
    *v == DEFAULT_MAX_FILES
}

impl ClipboardSection {
    /// True when this is all defaults (omitted from TOML).
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn is_default(&self) -> bool {
        !self.rich
            && !self.dnd
            && is_default_confirm(&self.confirm_mib)
            && is_default_max(&self.max_mib)
            && is_default_drop(&self.guest_drop)
            && is_default_files(&self.max_files)
    }

    /// Range checks.
    #[tracing::instrument(level = "debug", target = "ternvale::config", skip_all)]
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.confirm_mib == 0 || self.max_mib == 0 || self.confirm_mib > self.max_mib {
            return Err(ConfigError::InvalidClipboard {
                field: "clipboard.confirm_mib",
                reason: format!(
                    "confirm_mib ({}) must be 1..={}",
                    self.confirm_mib, self.max_mib
                ),
            });
        }
        if self.max_mib > 4096 {
            return Err(ConfigError::InvalidClipboard {
                field: "clipboard.max_mib",
                reason: "must be at most 4096".into(),
            });
        }
        if self.max_files == 0 || self.max_files > 10_000 {
            return Err(ConfigError::InvalidClipboard {
                field: "clipboard.max_files",
                reason: "must be 1..=10000".into(),
            });
        }
        match self.guest_drop.as_str() {
            "desktop" | "downloads" | "documents" => {}
            other => {
                return Err(ConfigError::InvalidClipboard {
                    field: "clipboard.guest_drop",
                    reason: format!("{other} is not desktop, downloads, or documents"),
                });
            }
        }
        Ok(())
    }

    /// Bytes at which the UI must prompt.
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn confirm_bytes(&self) -> u64 {
        self.confirm_mib.saturating_mul(1024 * 1024)
    }

    /// Hard cap in bytes.
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn max_bytes(&self) -> u64 {
        self.max_mib.saturating_mul(1024 * 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_opt_in_off() {
        let section = ClipboardSection::default();
        assert!(!section.rich);
        assert!(!section.dnd);
        assert!(section.is_default());
        section.validate().expect("defaults");
    }

    #[test]
    fn rejects_confirm_above_max() {
        let section = ClipboardSection {
            confirm_mib: 64,
            max_mib: 32,
            ..Default::default()
        };
        assert!(section.validate().is_err());
    }

    #[test]
    fn parses_opt_in_table() {
        let section: ClipboardSection = toml::from_str("rich = true\ndnd = true\n").expect("toml");
        assert!(section.rich);
        assert!(section.dnd);
        section.validate().expect("ok");
    }
}
