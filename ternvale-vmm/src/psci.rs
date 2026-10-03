//! PSCI 1.1 calls trapped from HVC or SMC.
//!
//! ARM ARM DDI 0487 defines the preferred exception return address for AArch64
//! HVC and SMC as the following instruction. On this host, `hv_vcpu_run` already
//! leaves `HV_REG_PC` there for HVC, and leaves it on the trapped instruction
//! for SMC (measured: `hvc` at `0x40000008` reported PC `0x4000000c`; `smc` at
//! the same address reported PC `0x40000008`). Only SMC is advanced by 4.
//! A trapped WFI or WFE leaves PC on that instruction; after the host wait,
//! the PC advances by 4 so the guest does not trap on it again.

use crate::vcpu::Vcpu;

/// Bytes to skip a trapped A64 instruction. HVC, SMC, WFI, and WFE are all 4 bytes.
pub const TRAP_PC_ADVANCE: u64 = 4;

const PSCI_VERSION: u64 = 0x8400_0000;
const PSCI_CPU_OFF: u64 = 0x8400_0002;
const PSCI_MIGRATE_INFO_TYPE: u64 = 0x8400_0006;
const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;
const PSCI_FEATURES: u64 = 0x8400_000a;
const PSCI_CPU_ON: u64 = 0xc400_0003;
const PSCI_AFFINITY_INFO: u64 = 0xc400_0004;
const SMCCC_FAST_CALL: u64 = 0x8000_0000;

/// PSCI return codes (DEN0022 table 5.2.2).
pub(crate) const SUCCESS: i32 = 0;
pub(crate) const NOT_SUPPORTED: i32 = -1;
pub(crate) const INVALID_PARAMS: i32 = -2;
pub(crate) const ALREADY_ON: i32 = -4;
pub(crate) const ON_PENDING: i32 = -5;
pub(crate) const INVALID_ADDRESS: i32 = -9;
/// `AFFINITY_INFO` states.
pub(crate) const AFFINITY_ON: i32 = 0;
pub(crate) const AFFINITY_OFF: i32 = 1;
pub(crate) const AFFINITY_ON_PENDING: i32 = 2;
/// PSCI 1.1, returned in x0 by `PSCI_VERSION`.
const VERSION_1_1: i32 = 0x0001_0001;
/// `MIGRATE_INFO_TYPE`: a trusted OS is not present and MP is allowed.
const TOS_NOT_PRESENT_MP: i32 = 2;

/// A PSCI call that depends on the power state of other vCPUs. The machine
/// answers it and writes the PSCI status to x0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerRequest {
    /// `CPU_ON`: start the CPU whose MPIDR affinity is `target` at `entry`
    /// with x0 = `context`.
    CpuOn {
        /// Target MPIDR (guest-supplied, unvalidated).
        target: u64,
        /// Physical entry point (guest-supplied, unvalidated).
        entry: u64,
        /// Value for the target's x0.
        context: u64,
    },
    /// `AFFINITY_INFO`: report the power state of `target`.
    AffinityInfo {
        /// Target MPIDR (guest-supplied, unvalidated).
        target: u64,
        /// Lowest affinity level the caller asks about.
        level: u64,
    },
}

/// What the vCPU loop should do after a PSCI call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsciAction {
    /// Write `x0` and skip the HVC or SMC.
    Return {
        /// PSCI status, sign-extended into x0.
        x0: u64,
    },
    /// The machine must answer this request. x0 is written by the caller.
    Power(PowerRequest),
    /// This vCPU should power off. x0 is `SUCCESS`.
    CpuOff,
    /// Power the VM off. `SYSTEM_OFF` does not return to the guest.
    SystemOff,
    /// Reset the VM. `SYSTEM_RESET` does not return to the guest.
    SystemReset,
}

/// Decode a PSCI function id. Any other SMCCC fast call returns
/// `NOT_SUPPORTED`. `None` means x0 is not an SMCCC fast call at all.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::psci",
    skip_all,
    fields(function = format!("{:#x}", function), x1 = format!("{:#x}", x1), x2 = format!("{:#x}", x2), x3 = format!("{:#x}", x3))
)]
pub fn call(function: u64, x1: u64, x2: u64, x3: u64) -> Option<PsciAction> {
    let action = match function {
        PSCI_VERSION => PsciAction::Return {
            x0: sign(VERSION_1_1),
        },
        PSCI_FEATURES => PsciAction::Return {
            x0: sign(features(x1)),
        },
        PSCI_CPU_ON => PsciAction::Power(PowerRequest::CpuOn {
            target: x1,
            entry: x2,
            context: x3,
        }),
        PSCI_AFFINITY_INFO => PsciAction::Power(PowerRequest::AffinityInfo {
            target: x1,
            level: x2,
        }),
        PSCI_CPU_OFF => PsciAction::CpuOff,
        PSCI_SYSTEM_OFF => PsciAction::SystemOff,
        PSCI_SYSTEM_RESET => PsciAction::SystemReset,
        PSCI_MIGRATE_INFO_TYPE => PsciAction::Return {
            x0: sign(TOS_NOT_PRESENT_MP),
        },
        _ if is_fast_call(function) => {
            tracing::warn!(
                target: "ternvale::psci",
                function = format!("{:#x}", function),
                "unknown smccc fast call; returning NOT_SUPPORTED"
            );
            PsciAction::Return {
                x0: sign(NOT_SUPPORTED),
            }
        }
        _ => return None,
    };
    let x0 = match action {
        PsciAction::Return { x0 } => x0,
        PsciAction::CpuOff => sign(SUCCESS),
        PsciAction::Power(_) | PsciAction::SystemOff | PsciAction::SystemReset => 0,
    };
    tracing::debug!(
        target: "ternvale::psci",
        function = format!("{:#x}", function),
        x1 = format!("{:#x}", x1),
        x2 = format!("{:#x}", x2),
        x3 = format!("{:#x}", x3),
        ret = format!("{:#x}", x0),
        ?action,
        "psci call"
    );
    Some(action)
}

/// Move PC past a trapped SMC, WFI, or WFE. HVC is already on the next instruction.
#[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(vcpu_id = vcpu.id()))]
pub fn advance_pc(vcpu: &Vcpu) -> Result<(), crate::vcpu::VcpuError> {
    let pc = vcpu.get_pc()?;
    let next = pc.wrapping_add(TRAP_PC_ADVANCE);
    tracing::debug!(
        target: "ternvale::psci",
        pc = format!("{:#x}", pc),
        next = format!("{:#x}", next),
        "advanced PC past trapped instruction"
    );
    vcpu.set_pc(next)
}

fn features(id: u64) -> i32 {
    match id {
        PSCI_VERSION
        | PSCI_CPU_OFF
        | PSCI_CPU_ON
        | PSCI_AFFINITY_INFO
        | PSCI_SYSTEM_OFF
        | PSCI_SYSTEM_RESET
        | PSCI_MIGRATE_INFO_TYPE
        | PSCI_FEATURES => SUCCESS,
        _ => NOT_SUPPORTED,
    }
}

/// SMCCC fast call: bit 31 of the 32-bit function id (W0). An unknown one
/// returns `NOT_SUPPORTED` (DEN0028 SMCCC v1.5 §2.5.3, "Unknown Function
/// Identifier"), as QEMU/KVM do. EDK2's `ArmTrngLib` probes `SMCCC_VERSION`
/// and `TRNG_VERSION` this way and continues without a TRNG.
fn is_fast_call(function: u64) -> bool {
    function >> 32 == 0 && function & SMCCC_FAST_CALL != 0
}

/// A PSCI status as the guest sees it in x0.
pub(crate) fn sign(code: i32) -> u64 {
    i64::from(code) as u64
}

#[cfg(test)]
mod tests {
    use super::{
        call, sign, NOT_SUPPORTED, PSCI_AFFINITY_INFO, PSCI_CPU_OFF, PSCI_CPU_ON, PSCI_FEATURES,
        PSCI_MIGRATE_INFO_TYPE, PSCI_SYSTEM_OFF, PSCI_SYSTEM_RESET, PSCI_VERSION, SUCCESS,
        TOS_NOT_PRESENT_MP, TRAP_PC_ADVANCE, VERSION_1_1,
    };
    use crate::psci::{PowerRequest, PsciAction};

    /// One row is a function id plus x1–x3, and the value `call` returns.
    /// `Return.x0` is the PSCI code the guest sees in x0.
    struct Row {
        function: u64,
        x1: u64,
        x2: u64,
        x3: u64,
        expected: Option<PsciAction>,
    }

    #[test]
    fn function_id_maps_to_the_psci_return() {
        let supported = Some(PsciAction::Return { x0: sign(SUCCESS) });
        let table = [
            Row {
                function: PSCI_VERSION,
                x1: 0,
                x2: 0,
                x3: 0,
                expected: Some(PsciAction::Return {
                    x0: sign(VERSION_1_1),
                }),
            },
            Row {
                function: PSCI_FEATURES,
                x1: PSCI_VERSION,
                x2: 0,
                x3: 0,
                expected: supported,
            },
            Row {
                function: PSCI_FEATURES,
                x1: PSCI_CPU_OFF,
                x2: 0,
                x3: 0,
                expected: supported,
            },
            Row {
                function: PSCI_FEATURES,
                x1: PSCI_CPU_ON,
                x2: 0,
                x3: 0,
                expected: supported,
            },
            Row {
                function: PSCI_FEATURES,
                x1: PSCI_SYSTEM_OFF,
                x2: 0,
                x3: 0,
                expected: supported,
            },
            Row {
                function: PSCI_FEATURES,
                x1: PSCI_SYSTEM_RESET,
                x2: 0,
                x3: 0,
                expected: supported,
            },
            Row {
                function: PSCI_FEATURES,
                x1: PSCI_MIGRATE_INFO_TYPE,
                x2: 0,
                x3: 0,
                expected: supported,
            },
            Row {
                function: PSCI_FEATURES,
                x1: PSCI_FEATURES,
                x2: 0,
                x3: 0,
                expected: supported,
            },
            Row {
                function: PSCI_FEATURES,
                x1: 0x8400_0001,
                x2: 0,
                x3: 0,
                expected: Some(PsciAction::Return {
                    x0: sign(NOT_SUPPORTED),
                }),
            },
            Row {
                function: PSCI_FEATURES,
                x1: PSCI_AFFINITY_INFO,
                x2: 0,
                x3: 0,
                expected: supported,
            },
            Row {
                function: PSCI_CPU_ON,
                x1: 1,
                x2: 0x4000_0000,
                x3: 9,
                expected: Some(PsciAction::Power(PowerRequest::CpuOn {
                    target: 1,
                    entry: 0x4000_0000,
                    context: 9,
                })),
            },
            Row {
                function: PSCI_AFFINITY_INFO,
                x1: 2,
                x2: 0,
                x3: 0,
                expected: Some(PsciAction::Power(PowerRequest::AffinityInfo {
                    target: 2,
                    level: 0,
                })),
            },
            Row {
                function: PSCI_CPU_OFF,
                x1: 0,
                x2: 0,
                x3: 0,
                expected: Some(PsciAction::CpuOff),
            },
            Row {
                function: PSCI_SYSTEM_OFF,
                x1: 0,
                x2: 0,
                x3: 0,
                expected: Some(PsciAction::SystemOff),
            },
            Row {
                function: PSCI_SYSTEM_RESET,
                x1: 0,
                x2: 0,
                x3: 0,
                expected: Some(PsciAction::SystemReset),
            },
            Row {
                function: PSCI_MIGRATE_INFO_TYPE,
                x1: 0,
                x2: 0,
                x3: 0,
                expected: Some(PsciAction::Return {
                    x0: sign(TOS_NOT_PRESENT_MP),
                }),
            },
            Row {
                function: 0,
                x1: 0,
                x2: 0,
                x3: 0,
                expected: None,
            },
            Row {
                function: 0x1_8000_0000,
                x1: 0,
                x2: 0,
                x3: 0,
                expected: None,
            },
        ];
        // SMCCC_VERSION, ARCH_FEATURES, TRNG_VERSION, TRNG_RND64, a vendor hyp call.
        for function in [
            0x8000_0000,
            0x8000_0001,
            0x8400_0050,
            0xc400_0053,
            0x8600_ff01,
        ] {
            assert_eq!(
                call(function, 0, 0, 0),
                Some(PsciAction::Return {
                    x0: sign(NOT_SUPPORTED)
                }),
                "function {function:#x}"
            );
        }
        for row in table {
            assert_eq!(
                call(row.function, row.x1, row.x2, row.x3),
                row.expected,
                "function {:#x}",
                row.function
            );
        }
    }

    #[test]
    fn smc_and_wfi_advance_four_bytes() {
        assert_eq!(TRAP_PC_ADVANCE, 4);
    }
}
