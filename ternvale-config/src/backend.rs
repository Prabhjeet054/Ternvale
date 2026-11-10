//! Top-level VM hypervisor backend (`backend = "native" | "vz" | "auto"`).
//! Distinct from `[tpm].backend`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Which hypervisor stack should run this VM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// Choose from the guest OS and features ([`ternvale_backend::select`]).
    #[default]
    Auto,
    /// Hypervisor.framework orchestrator (Parts 1–3).
    Native,
    /// Virtualization.framework (Step 92).
    Vz,
}

impl fmt::Display for BackendKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Native => "native",
            Self::Vz => "vz",
        })
    }
}

impl BackendKind {
    /// `auto` is the default and is omitted from TOML.
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn is_auto(&self) -> bool {
        *self == Self::Auto
    }
}

#[cfg(test)]
mod tests {
    use super::BackendKind;

    #[test]
    fn names_round_trip() {
        assert_eq!(BackendKind::Auto.to_string(), "auto");
        assert_eq!(BackendKind::Native.to_string(), "native");
        assert_eq!(BackendKind::Vz.to_string(), "vz");
        assert!(BackendKind::Auto.is_auto());
        assert!(!BackendKind::Native.is_auto());
    }
}
