//! Fixed ACPI Description Table for a hardware-reduced ARM guest.
//!
//! ACPI 6.5 §5.2.9 (Table 5.9): signature `FACP`, revision 6, 276 bytes.
//! Hardware-reduced ACPI (§4.1) has no fixed hardware, so every PM block,
//! GPE block, SCI, SMI command, sleep register, and reset register field stays
//! zero. Only these are set:
//!
//! - `Flags` bit 20 `HW_REDUCED_ACPI` (§5.2.9, Fixed ACPI Description Table
//!   Fixed Feature Flags). Linux arm64 refuses ACPI without it
//!   (`acpi_fadt_sanity_check` in `arch/arm64/kernel/acpi.c`).
//! - `ARM_BOOT_ARCH` bit 0 `PSCI_COMPLIANT` and bit 1 `PSCI_USE_HVC`
//!   (§5.2.9.4, ARM Architecture Boot Flags), matching the DTB's
//!   `psci` node `method = "hvc"`.
//! - `FADT Minor Version` 5 (ACPI 6.5).
//! - `X_DSDT`, the 64-bit DSDT address. The 32-bit `DSDT` field stays zero:
//!   OSPM ignores it when `X_DSDT` is non-zero (§5.2.9), and QEMU's `virt`
//!   does the same.
//!
//! TODO(verify): Windows on Arm may expect specific FADT minor versions or
//! `Preferred_PM_Profile` values; check against the Windows ACPI requirements
//! ("Hardware-reduced ACPI" / SBSA platform guidance) before Windows boots.

use crate::sdt::{table, SdtHeader, SDT_HEADER_LEN};
use crate::AcpiError;

/// FADT signature (`FACP`, not `FADT`).
pub const FADT_SIGNATURE: [u8; 4] = *b"FACP";
/// FADT major revision for ACPI 6.x.
pub const FADT_REVISION: u8 = 6;
/// FADT minor revision for ACPI 6.5.
pub const FADT_MINOR_REVISION: u8 = 5;
/// Bytes in a revision 6 FADT.
pub const FADT_LEN: usize = 276;
/// `Flags` bit 20: no fixed ACPI hardware.
pub const HW_REDUCED_ACPI: u32 = 1 << 20;
/// `ARM_BOOT_ARCH` bit 0: PSCI is implemented.
pub const ARM_PSCI_COMPLIANT: u16 = 1 << 0;
/// `ARM_BOOT_ARCH` bit 1: call PSCI with HVC instead of SMC.
pub const ARM_PSCI_USE_HVC: u16 = 1 << 1;

/// Byte offset of `DSDT` (32-bit).
pub const DSDT_OFFSET: usize = 40;
/// Byte offset of `Flags`.
pub const FLAGS_OFFSET: usize = 112;
/// Byte offset of `ARM_BOOT_ARCH`.
pub const ARM_BOOT_ARCH_OFFSET: usize = 129;
/// Byte offset of `FADT Minor Version`.
pub const MINOR_VERSION_OFFSET: usize = 131;
/// Byte offset of `X_DSDT`.
pub const X_DSDT_OFFSET: usize = 140;
/// Byte offset of `Hypervisor Vendor Identity`, the last field.
pub const HYPERVISOR_VENDOR_OFFSET: usize = 268;

/// How the guest calls PSCI firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsciConduit {
    /// `HVC #0`, handled by the VMM (Ternvale's only conduit).
    Hvc,
    /// `SMC #0`, handled by EL3 firmware.
    Smc,
}

/// A hardware-reduced FADT pointing at the DSDT at `dsdt_gpa`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(dsdt = %format!("{dsdt_gpa:#x}"), conduit = ?conduit)
)]
pub fn fadt(dsdt_gpa: u64, conduit: PsciConduit) -> Result<Vec<u8>, AcpiError> {
    let mut body = vec![0u8; FADT_LEN - SDT_HEADER_LEN];
    let at = |offset: usize| offset - SDT_HEADER_LEN;
    body[at(FLAGS_OFFSET)..at(FLAGS_OFFSET) + 4].copy_from_slice(&HW_REDUCED_ACPI.to_le_bytes());
    let boot_arch = match conduit {
        PsciConduit::Hvc => ARM_PSCI_COMPLIANT | ARM_PSCI_USE_HVC,
        PsciConduit::Smc => ARM_PSCI_COMPLIANT,
    };
    body[at(ARM_BOOT_ARCH_OFFSET)..at(ARM_BOOT_ARCH_OFFSET) + 2]
        .copy_from_slice(&boot_arch.to_le_bytes());
    body[at(MINOR_VERSION_OFFSET)] = FADT_MINOR_REVISION;
    body[at(X_DSDT_OFFSET)..at(X_DSDT_OFFSET) + 8].copy_from_slice(&dsdt_gpa.to_le_bytes());
    tracing::debug!(
        target: "ternvale::acpi",
        flags = %format!("{HW_REDUCED_ACPI:#x}"),
        arm_boot_arch = %format!("{boot_arch:#x}"),
        x_dsdt = %format!("{dsdt_gpa:#x}"),
        "fadt fields"
    );
    table(SdtHeader::new(FADT_SIGNATURE, FADT_REVISION), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdt::byte_sum;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn fadt_is_hardware_reduced_revision_6_5() {
        let bytes = fadt(0x0910_0200, PsciConduit::Hvc).expect("fadt");
        assert_eq!(bytes.len(), 276);
        let header = SdtHeader::parse(&bytes).expect("header");
        assert_eq!(&header.signature, b"FACP");
        assert_eq!(header.length, 276);
        assert_eq!(header.revision, 6);
        assert_eq!(bytes[MINOR_VERSION_OFFSET], 5);
        assert_eq!(byte_sum(&bytes), 0);
        assert_eq!(u32_at(&bytes, FLAGS_OFFSET), 1 << 20);
    }

    #[test]
    fn fadt_points_at_the_dsdt_through_x_dsdt_only() {
        let bytes = fadt(0x1_2345_6780, PsciConduit::Hvc).expect("fadt");
        assert_eq!(u32_at(&bytes, DSDT_OFFSET), 0);
        assert_eq!(
            u64::from_le_bytes(bytes[X_DSDT_OFFSET..X_DSDT_OFFSET + 8].try_into().unwrap()),
            0x1_2345_6780
        );
    }

    #[test]
    fn arm_boot_flags_follow_the_conduit() {
        let hvc = fadt(0, PsciConduit::Hvc).expect("hvc");
        assert_eq!(u16_at(&hvc, ARM_BOOT_ARCH_OFFSET), 0b11);
        let smc = fadt(0, PsciConduit::Smc).expect("smc");
        assert_eq!(u16_at(&smc, ARM_BOOT_ARCH_OFFSET), 0b01);
    }

    #[test]
    fn fixed_hardware_fields_stay_zero() {
        let bytes = fadt(0x4000, PsciConduit::Hvc).expect("fadt");
        // Reserved, Preferred_PM_Profile, SCI_INT .. RESET_VALUE (44..129), except Flags.
        for (offset, byte) in bytes.iter().enumerate().take(ARM_BOOT_ARCH_OFFSET).skip(44) {
            if (FLAGS_OFFSET..FLAGS_OFFSET + 4).contains(&offset) {
                continue;
            }
            assert_eq!(*byte, 0, "offset {offset}");
        }
        // X_PM1a_EVT_BLK .. Hypervisor Vendor Identity (148..276).
        assert!(bytes[148..].iter().all(|byte| *byte == 0));
        assert_eq!(HYPERVISOR_VENDOR_OFFSET + 8, FADT_LEN);
    }
}
