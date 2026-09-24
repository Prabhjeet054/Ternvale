//! `hv_return_t` names from the arm64 kernel header.
//!
//! Values are from `usr/include/arm64/hv/hv_kern_types.h` in the macOS 27 SDK.
//! `err_common_hypervisor` is `err_local | err_sub(0xba5)`, and the header
//! comments give the resulting constants (for example `HV_ERROR` is
//! `0xfae94001`). The x86 `hv_error.h` uses `HV_FAULT` for `0xfae94008`; arm64
//! names that code `HV_EXISTS` and adds `HV_ILLEGAL_GUEST_STATE`.

use thiserror::Error;

/// `HV_SUCCESS`.
pub const HV_SUCCESS: i32 = 0;
/// `HV_ERROR` (`0xfae94001`).
pub const HV_ERROR: i32 = 0xfae9_4001_u32 as i32;
/// `HV_BUSY` (`0xfae94002`).
pub const HV_BUSY: i32 = 0xfae9_4002_u32 as i32;
/// `HV_BAD_ARGUMENT` (`0xfae94003`).
pub const HV_BAD_ARGUMENT: i32 = 0xfae9_4003_u32 as i32;
/// `HV_ILLEGAL_GUEST_STATE` (`0xfae94004`).
pub const HV_ILLEGAL_GUEST_STATE: i32 = 0xfae9_4004_u32 as i32;
/// `HV_NO_RESOURCES` (`0xfae94005`).
pub const HV_NO_RESOURCES: i32 = 0xfae9_4005_u32 as i32;
/// `HV_NO_DEVICE` (`0xfae94006`).
pub const HV_NO_DEVICE: i32 = 0xfae9_4006_u32 as i32;
/// `HV_DENIED` (`0xfae94007`).
pub const HV_DENIED: i32 = 0xfae9_4007_u32 as i32;
/// `HV_EXISTS` (`0xfae94008`).
pub const HV_EXISTS: i32 = 0xfae9_4008_u32 as i32;
/// `HV_UNSUPPORTED` (`0xfae9400f`).
pub const HV_UNSUPPORTED: i32 = 0xfae9_400f_u32 as i32;

/// A failed Hypervisor.framework call, or a second `Vm::create` in this process.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HvError {
    /// `HV_ERROR`.
    #[error("HV_ERROR ({code:#x})")]
    Error {
        /// Raw `hv_return_t`.
        code: i32,
    },
    /// `HV_BUSY`.
    #[error("HV_BUSY ({code:#x})")]
    Busy {
        /// Raw `hv_return_t`.
        code: i32,
    },
    /// `HV_BAD_ARGUMENT`.
    #[error("HV_BAD_ARGUMENT ({code:#x})")]
    BadArgument {
        /// Raw `hv_return_t`.
        code: i32,
    },
    /// `HV_ILLEGAL_GUEST_STATE`.
    #[error("HV_ILLEGAL_GUEST_STATE ({code:#x})")]
    IllegalGuestState {
        /// Raw `hv_return_t`.
        code: i32,
    },
    /// `HV_NO_RESOURCES`.
    #[error("HV_NO_RESOURCES ({code:#x})")]
    NoResources {
        /// Raw `hv_return_t`.
        code: i32,
    },
    /// `HV_NO_DEVICE`.
    #[error("HV_NO_DEVICE ({code:#x})")]
    NoDevice {
        /// Raw `hv_return_t`.
        code: i32,
    },
    /// `HV_DENIED`.
    #[error("HV_DENIED ({code:#x})")]
    Denied {
        /// Raw `hv_return_t`.
        code: i32,
    },
    /// `HV_EXISTS`.
    #[error("HV_EXISTS ({code:#x})")]
    Exists {
        /// Raw `hv_return_t`.
        code: i32,
    },
    /// `HV_UNSUPPORTED`.
    #[error("HV_UNSUPPORTED ({code:#x})")]
    Unsupported {
        /// Raw `hv_return_t`.
        code: i32,
    },
    /// A code that is not in the arm64 header enum.
    #[error("unknown hv_return_t ({code:#x})")]
    Unknown {
        /// Raw `hv_return_t`.
        code: i32,
    },
    /// `Vm::create` was called while another `Vm` is still alive.
    #[error("a VM already exists in this process")]
    AlreadyExists,
    /// A general-purpose register index was outside `0..=30`.
    #[error("gpr index {index} is outside 0..=30")]
    InvalidGpr {
        /// The rejected `Xn` index.
        index: u8,
    },
    /// In-kernel GICv3 needs macOS 15.0 (`API_AVAILABLE(macos(15.0))` in `hv_gic.h`).
    #[error("in-kernel GICv3 requires macOS 15.0 or newer; host is {major}.{minor}")]
    GicOsUnsupported {
        /// Host `kern.osproductversion` major.
        major: u32,
        /// Host `kern.osproductversion` minor.
        minor: u32,
    },
    /// The GICv3 symbols are missing from Hypervisor.framework.
    #[error("in-kernel GICv3 is not available in this Hypervisor.framework")]
    GicUnavailable,
    /// `hv_gic_create` ran before `hv_vm_create`.
    #[error("GIC must be created after hv_vm_create")]
    GicBeforeVm,
    /// A vCPU already exists, so the GIC window has closed.
    #[error("GIC must be created before any vCPU")]
    GicAfterVcpu,
    /// This VM already has a GIC.
    #[error("a GIC already exists for this VM")]
    GicExists,
    /// `hv_vcpu_create` ran before `hv_gic_create`.
    #[error("vCPU requires an in-kernel GIC; create the GIC first")]
    VcpuBeforeGic,
    /// Distributor or redistributor GPA is not aligned to the framework requirement.
    #[error("GIC {region} base {base:#x} is not aligned to {alignment:#x}")]
    GicMisaligned {
        /// `distributor` or `redistributor`.
        region: &'static str,
        /// Guest physical address that was rejected.
        base: u64,
        /// Alignment in bytes reported by the framework.
        alignment: usize,
    },
    /// `hv_gic_config_create` or `hv_gic_state_create` returned NULL.
    #[error("Hypervisor.framework returned a null {what}")]
    GicNull {
        /// Which object was null.
        what: &'static str,
    },
    /// `sysctlbyname(kern.osproductversion)` failed or the string was not a version.
    #[error("could not read the macOS version")]
    OsVersion,
}

impl HvError {
    /// Map a non-success `hv_return_t` to its name.
    ///
    /// `HV_SUCCESS` is not an error. Callers check for zero before using this.
    #[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(code = code))]
    pub fn from_code(code: i32) -> Self {
        match code {
            HV_ERROR => Self::Error { code },
            HV_BUSY => Self::Busy { code },
            HV_BAD_ARGUMENT => Self::BadArgument { code },
            HV_ILLEGAL_GUEST_STATE => Self::IllegalGuestState { code },
            HV_NO_RESOURCES => Self::NoResources { code },
            HV_NO_DEVICE => Self::NoDevice { code },
            HV_DENIED => Self::Denied { code },
            HV_EXISTS => Self::Exists { code },
            HV_UNSUPPORTED => Self::Unsupported { code },
            _ => {
                // TODO(verify): the arm64 header lists only the codes above.
                // Confirm the kernel never returns another hv_return_t.
                tracing::warn!(
                    target: "ternvale::hv",
                    code = format!("{:#x}", code),
                    "unmapped hv_return_t"
                );
                Self::Unknown { code }
            }
        }
    }
}
