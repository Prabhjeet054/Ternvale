//! Root System Description Pointer.
//!
//! ACPI 6.5 §5.2.5.3 (Table 5.3), revision 2 (ACPI 2.0 and later). On a UEFI
//! system the OS finds it through the EFI configuration table (§5.2.5.2), so
//! it has no alignment or location rule beyond what firmware imposes.
//! Ternvale lists tables only through the XSDT; `RsdtAddress` stays zero as in
//! QEMU's `virt` machine (Arm BBR requires the XSDT on AArch64).
//! TODO(verify): Arm BBR 2.x "ACPI requirements" wording on RsdtAddress being
//! zero versus ignored.

use crate::sdt::{byte_sum, checksum, OEM_ID};

/// Bytes in a revision 2 RSDP.
pub const RSDP_LEN: usize = 36;
/// "RSD PTR " (note the trailing space).
pub const RSDP_SIGNATURE: [u8; 8] = *b"RSD PTR ";
/// Revision 2: XSDT address and extended checksum present.
pub const RSDP_REVISION: u8 = 2;
/// Bytes covered by the first (ACPI 1.0) checksum.
pub const RSDP_V1_LEN: usize = 20;
/// Offset of the ACPI 1.0 checksum.
pub const RSDP_CHECKSUM_OFFSET: usize = 8;
/// Offset of the extended checksum over all 36 bytes.
pub const RSDP_EXT_CHECKSUM_OFFSET: usize = 32;
/// Offset of `XsdtAddress`.
pub const RSDP_XSDT_OFFSET: usize = 24;

/// A revision 2 RSDP pointing at the XSDT at `xsdt_gpa`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(xsdt = %format!("{xsdt_gpa:#x}"))
)]
pub fn rsdp(xsdt_gpa: u64) -> [u8; RSDP_LEN] {
    let mut out = [0u8; RSDP_LEN];
    out[0..8].copy_from_slice(&RSDP_SIGNATURE);
    out[9..15].copy_from_slice(&OEM_ID);
    out[15] = RSDP_REVISION;
    // RsdtAddress (16..20) stays zero; see the module comment.
    out[20..24].copy_from_slice(&(RSDP_LEN as u32).to_le_bytes());
    out[RSDP_XSDT_OFFSET..RSDP_XSDT_OFFSET + 8].copy_from_slice(&xsdt_gpa.to_le_bytes());
    out[RSDP_CHECKSUM_OFFSET] = checksum(&out[..RSDP_V1_LEN]);
    out[RSDP_EXT_CHECKSUM_OFFSET] = checksum(&out);
    tracing::debug!(
        target: "ternvale::acpi",
        checksum = %format!("{:#04x}", out[RSDP_CHECKSUM_OFFSET]),
        extended_checksum = %format!("{:#04x}", out[RSDP_EXT_CHECKSUM_OFFSET]),
        "rsdp built"
    );
    out
}

/// True when both RSDP checksums hold.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
pub fn rsdp_checksums_ok(bytes: &[u8; RSDP_LEN]) -> bool {
    byte_sum(&bytes[..RSDP_V1_LEN]) == 0 && byte_sum(bytes) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsdp_follows_table_5_3() {
        let bytes = rsdp(0x0910_0040);
        assert_eq!(&bytes[0..8], b"RSD PTR ");
        assert_eq!(&bytes[9..15], b"TERNVL");
        assert_eq!(bytes[15], 2);
        assert_eq!(&bytes[16..20], &[0; 4], "RsdtAddress");
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 36);
        assert_eq!(
            u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            0x0910_0040
        );
        assert_eq!(&bytes[33..36], &[0; 3], "reserved");
        assert!(rsdp_checksums_ok(&bytes));
    }

    #[test]
    fn both_checksums_are_checked() {
        let mut bytes = rsdp(0x1000);
        bytes[RSDP_XSDT_OFFSET] ^= 1;
        assert!(!rsdp_checksums_ok(&bytes), "extended checksum ignored");
        let mut bytes = rsdp(0x1000);
        bytes[RSDP_CHECKSUM_OFFSET] ^= 1;
        bytes[RSDP_EXT_CHECKSUM_OFFSET] ^= 1;
        assert!(!rsdp_checksums_ok(&bytes), "v1 checksum ignored");
    }
}
