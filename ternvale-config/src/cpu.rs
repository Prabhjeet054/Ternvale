//! Guest CPU feature profile (`cpu_profile` in the VM TOML).

use serde::{Deserialize, Serialize};

/// How Ternvale presents ID_AA64* to the guest.
///
/// Hypervisor.framework exposes **get** (`hv_vcpu_config_get_feature_reg`) and
/// no `set_feature_reg`. `baseline` still tries `hv_vcpu_set_sys_reg` on ID
/// registers after vCPU create; if the kernel rejects a write, the host value
/// stays and restore uses detection + refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CpuProfile {
    /// Pass through whatever this Mac's Hypervisor.framework reports.
    #[default]
    Host,
    /// Restrict optional ISA extensions (SVE/SME and later ID banks) when
    /// `hv_vcpu_set_sys_reg` accepts the write.
    Baseline,
}

impl CpuProfile {
    /// Wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Baseline => "baseline",
        }
    }

    /// Default TOML omits the key.
    #[must_use]
    pub fn is_host(&self) -> bool {
        matches!(self, Self::Host)
    }
}

impl std::fmt::Display for CpuProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::CpuProfile;

    #[test]
    fn host_is_default() {
        assert_eq!(CpuProfile::default(), CpuProfile::Host);
        assert!(CpuProfile::Host.is_host());
        assert!(!CpuProfile::Baseline.is_host());
        assert_eq!(CpuProfile::Baseline.as_str(), "baseline");
    }
}
