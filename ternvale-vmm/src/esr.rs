//! ESR_EL2 decoding for a vCPU exception exit.
//!
//! Field positions follow ARM ARM DDI 0487: EC is bits \[31:26\], ISS is bits
//! \[24:0\]. Data-abort ISS: ISV is bit 24, SAS is \[23:22\], SRT is \[20:16\],
//! SF is bit 15, and WnR is bit 6. SAS `0b00` is 1 byte, then 2, 4, and 8.
//!
//! A data abort with ISV clear does not carry a valid size or register, so it
//! stays [`ExitEvent::Unknown`]. Instruction aborts (`0x20`/`0x21`) and BRK
//! (`0x3C`) also stay unknown: this step has no variant for them.
//!
//! On this host, HVC already leaves `HV_REG_PC` on the following instruction.
//! SMC leaves it on the trapped instruction, so only SMC is advanced by 4.
//! The ARM ARM preferred return address for both is the following instruction.

use std::fmt;

const EC_WFI: u64 = 0x01;
const EC_HVC64: u64 = 0x16;
const EC_SMC64: u64 = 0x17;
const EC_SYSREG: u64 = 0x18;
const EC_IABT_LOWER: u64 = 0x20;
const EC_IABT_SAME: u64 = 0x21;
const EC_DABT_LOWER: u64 = 0x24;
const EC_DABT_SAME: u64 = 0x25;
const EC_BRK: u64 = 0x3c;

/// A decoded guest exit. `gpa` for [`ExitEvent::Mmio`] comes from the
/// hypervisor exit record, not from the ESR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitEvent {
    /// Data abort with ISV set. `size` is the access width in bytes.
    Mmio {
        /// Guest physical address of the access.
        gpa: u64,
        /// Access size in bytes: 1, 2, 4, or 8.
        size: u8,
        /// `WnR`: the guest was writing.
        write: bool,
        /// `SRT`, the general-purpose register in the instruction.
        reg: u8,
    },
    /// AArch64 `HVC`. `imm16` is ISS bits \[15:0\].
    Hvc {
        /// Immediate from the HVC instruction.
        imm16: u16,
    },
    /// AArch64 `SMC`. `imm16` is ISS bits \[15:0\].
    Smc {
        /// Immediate from the SMC instruction.
        imm16: u16,
    },
    /// `WFI` or `WFE` (EC `0x01`). `wfe` is the ISS TI bit.
    Wfi {
        /// ISS bit 0. Set for `WFE`, clear for `WFI`.
        wfe: bool,
    },
    /// Trapped `MSR`/`MRS` (EC `0x18`).
    SysReg {
        /// `Op0`.
        op0: u8,
        /// `Op1`.
        op1: u8,
        /// `CRn`.
        crn: u8,
        /// `CRm`.
        crm: u8,
        /// `Op2`.
        op2: u8,
        /// Transfer register `Rt`.
        reg: u8,
        /// Guest used `MSR` (a write of the system register). `MRS` is a read.
        write: bool,
    },
    /// An EC this decoder does not turn into a specific event, or a data abort
    /// whose ISS is not valid.
    Unknown {
        /// Exception class, bits \[31:26\].
        ec: u8,
        /// Instruction specific syndrome, bits \[24:0\].
        iss: u32,
    },
}

/// Decode `esr` (ESR_EL2). `gpa` is the guest physical address from the exit.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::vcpu",
    skip_all,
    fields(esr = format!("{:#x}", esr), gpa = format!("{:#x}", gpa))
)]
pub fn decode(esr: u64, gpa: u64) -> ExitEvent {
    let ec = (esr >> 26) & 0x3f;
    let iss = (esr & 0x1ff_ffff) as u32;
    let event = match ec {
        EC_WFI => ExitEvent::Wfi { wfe: iss & 1 == 1 },
        EC_HVC64 => ExitEvent::Hvc {
            imm16: (iss & 0xffff) as u16,
        },
        EC_SMC64 => ExitEvent::Smc {
            imm16: (iss & 0xffff) as u16,
        },
        EC_SYSREG => decode_sysreg(iss),
        EC_DABT_LOWER | EC_DABT_SAME => decode_data_abort(ec as u8, iss, gpa),
        EC_IABT_LOWER | EC_IABT_SAME | EC_BRK => ExitEvent::Unknown { ec: ec as u8, iss },
        _ => ExitEvent::Unknown { ec: ec as u8, iss },
    };
    log_event(esr, &event);
    event
}

fn decode_sysreg(iss: u32) -> ExitEvent {
    // Direction bit 0: 0 is MSR (write to the system register), 1 is MRS.
    ExitEvent::SysReg {
        op0: ((iss >> 20) & 0x3) as u8,
        op1: ((iss >> 14) & 0x7) as u8,
        crn: ((iss >> 10) & 0xf) as u8,
        crm: ((iss >> 1) & 0xf) as u8,
        op2: ((iss >> 17) & 0x7) as u8,
        reg: ((iss >> 5) & 0x1f) as u8,
        write: iss & 1 == 0,
    }
}

fn decode_data_abort(ec: u8, iss: u32, gpa: u64) -> ExitEvent {
    let isv = (iss >> 24) & 1 == 1;
    let sas = (iss >> 22) & 0x3;
    let sf = (iss >> 15) & 1 == 1;
    let reg = ((iss >> 16) & 0x1f) as u8;
    let write = (iss >> 6) & 1 == 1;
    if !isv {
        tracing::warn!(
            target: "ternvale::vcpu",
            iss = format!("{:#x}", iss),
            "data abort ISS is not valid"
        );
        return ExitEvent::Unknown { ec, iss };
    }
    let size = 1u8 << sas;
    tracing::trace!(
        target: "ternvale::vcpu",
        isv,
        sas,
        sf,
        srt = reg,
        wnr = write,
        gpa = format!("{:#x}", gpa),
        size,
        "decoded data abort"
    );
    ExitEvent::Mmio {
        gpa,
        size,
        write,
        reg,
    }
}

fn log_event(esr: u64, event: &ExitEvent) {
    match event {
        ExitEvent::Unknown { ec, iss } => tracing::error!(
            target: "ternvale::vcpu",
            esr = format!("{:#x}", esr),
            ec = format!("{:#x}", ec),
            iss = format!("{:#x}", iss),
            "unknown esr"
        ),
        other => tracing::trace!(
            target: "ternvale::vcpu",
            esr = format!("{:#x}", esr),
            event = %DisplayEvent(other),
            "decoded esr"
        ),
    }
}

/// One-line field dump so the TRACE line lists every decoded field.
struct DisplayEvent<'a>(&'a ExitEvent);

impl fmt::Display for DisplayEvent<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ExitEvent::Mmio {
                gpa,
                size,
                write,
                reg,
            } => write!(
                formatter,
                "mmio gpa={gpa:#x} size={size} write={write} reg={reg}"
            ),
            ExitEvent::Hvc { imm16 } => write!(formatter, "hvc imm16={imm16:#x}"),
            ExitEvent::Smc { imm16 } => write!(formatter, "smc imm16={imm16:#x}"),
            ExitEvent::Wfi { wfe } => write!(formatter, "wfi wfe={wfe}"),
            ExitEvent::SysReg {
                op0,
                op1,
                crn,
                crm,
                op2,
                reg,
                write,
            } => write!(
                formatter,
                "sysreg op0={op0} op1={op1} crn={crn} crm={crm} op2={op2} reg={reg} write={write}"
            ),
            ExitEvent::Unknown { ec, iss } => write!(formatter, "unknown ec={ec:#x} iss={iss:#x}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{decode, ExitEvent};

    fn esr(ec: u64, iss: u64) -> u64 {
        (ec << 26) | (1 << 25) | (iss & 0x1ff_ffff)
    }

    #[test]
    fn decodes_each_ec_exactly() {
        let sys = (0x3 << 20) | (0x2 << 17) | (0x4 << 14) | (0x5 << 10) | (9 << 5) | (0x6 << 1);
        let write32 = (1 << 24) | (0b10 << 22) | (3 << 16) | (1 << 6);
        let read64 = (1 << 24) | (0b11 << 22) | (7 << 16) | (1 << 15);
        let cases = [
            ("wfi", esr(0x01, 0), 0, ExitEvent::Wfi { wfe: false }),
            ("wfe", esr(0x01, 1), 0, ExitEvent::Wfi { wfe: true }),
            ("hvc", esr(0x16, 0x42), 0, ExitEvent::Hvc { imm16: 0x42 }),
            ("smc", esr(0x17, 0x7), 0, ExitEvent::Smc { imm16: 0x7 }),
            (
                "sysreg",
                esr(0x18, sys),
                0,
                ExitEvent::SysReg {
                    op0: 3,
                    op1: 4,
                    crn: 5,
                    crm: 6,
                    op2: 2,
                    reg: 9,
                    write: true,
                },
            ),
            (
                "iabt-lower",
                esr(0x20, 0x11),
                0,
                ExitEvent::Unknown {
                    ec: 0x20,
                    iss: 0x11,
                },
            ),
            (
                "iabt-same",
                esr(0x21, 0),
                0,
                ExitEvent::Unknown { ec: 0x21, iss: 0 },
            ),
            (
                "write32-x3",
                esr(0x24, write32),
                0x0900_0000,
                ExitEvent::Mmio {
                    gpa: 0x0900_0000,
                    size: 4,
                    write: true,
                    reg: 3,
                },
            ),
            (
                "isv0",
                esr(0x24, 0),
                0x0900_0000,
                ExitEvent::Unknown { ec: 0x24, iss: 0 },
            ),
            (
                "read64-x7",
                esr(0x25, read64),
                0x4000,
                ExitEvent::Mmio {
                    gpa: 0x4000,
                    size: 8,
                    write: false,
                    reg: 7,
                },
            ),
            (
                "brk",
                esr(0x3c, 0x99),
                0,
                ExitEvent::Unknown {
                    ec: 0x3c,
                    iss: 0x99,
                },
            ),
            (
                "unknown-ec",
                esr(0x3f, 0x55),
                0,
                ExitEvent::Unknown {
                    ec: 0x3f,
                    iss: 0x55,
                },
            ),
        ];
        for (name, syndrome, gpa, expected) in cases {
            assert_eq!(decode(syndrome, gpa), expected, "{name}");
        }
    }
}
