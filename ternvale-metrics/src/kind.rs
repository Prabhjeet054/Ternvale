//! Exit classes counted per vCPU. Indices are stable for snapshots.

use serde::{Deserialize, Serialize};

/// How many [`ExitKind`] variants there are.
pub const EXIT_KINDS: usize = 10;

/// One class of hypervisor exit, matching [`ternvale_vmm::ExitEvent`] plus
/// timer / cancel / leftover reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ExitKind {
    /// Data-abort MMIO.
    Mmio = 0,
    /// `WFI`.
    Wfi = 1,
    /// `WFE`.
    Wfe = 2,
    /// `HVC`.
    Hvc = 3,
    /// `SMC`.
    Smc = 4,
    /// Trapped `MSR`/`MRS`.
    SysReg = 5,
    /// Virtual timer PPI.
    Vtimer = 6,
    /// Unrecognized ESR / exit reason.
    Unknown = 7,
    /// `hv_vcpus_exit`.
    Canceled = 8,
    /// Any other `ExitReason` (PSCI power, system-off already counted as HVC).
    Other = 9,
}

impl ExitKind {
    /// Every variant in index order.
    pub const ALL: [Self; EXIT_KINDS] = [
        Self::Mmio,
        Self::Wfi,
        Self::Wfe,
        Self::Hvc,
        Self::Smc,
        Self::SysReg,
        Self::Vtimer,
        Self::Unknown,
        Self::Canceled,
        Self::Other,
    ];

    /// Array index for this kind.
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// Stable snapshot field name.
    #[tracing::instrument(level = "debug", target = "ternvale::metrics", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mmio => "mmio",
            Self::Wfi => "wfi",
            Self::Wfe => "wfe",
            Self::Hvc => "hvc",
            Self::Smc => "smc",
            Self::SysReg => "sysreg",
            Self::Vtimer => "vtimer",
            Self::Unknown => "unknown",
            Self::Canceled => "canceled",
            Self::Other => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ExitKind, EXIT_KINDS};

    #[test]
    fn kinds_are_dense_indices() {
        for (i, kind) in ExitKind::ALL.iter().enumerate() {
            assert_eq!(kind.index(), i);
        }
        assert_eq!(EXIT_KINDS, ExitKind::ALL.len());
    }
}
