//! Extended System Description Table.
//!
//! ACPI 6.5 §5.2.8: an SDT header (signature `XSDT`, revision 1)
//! followed by 64-bit physical addresses of the other description tables.
//! The DSDT is not listed; the FADT points at it (§5.2.9 `X_DSDT`).

use crate::sdt::{table, SdtHeader};
use crate::AcpiError;

/// XSDT signature.
pub const XSDT_SIGNATURE: [u8; 4] = *b"XSDT";
/// XSDT revision (ACPI 6.5 §5.2.8).
pub const XSDT_REVISION: u8 = 1;

/// Bytes in an XSDT that lists `entries` tables.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(entries))]
pub fn xsdt_len(entries: usize) -> usize {
    crate::sdt::SDT_HEADER_LEN + entries * 8
}

/// An XSDT listing `entries` (guest physical table addresses) in order.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(entries = entries.len()))]
pub fn xsdt(entries: &[u64]) -> Result<Vec<u8>, AcpiError> {
    let body: Vec<u8> = entries.iter().flat_map(|gpa| gpa.to_le_bytes()).collect();
    table(SdtHeader::new(XSDT_SIGNATURE, XSDT_REVISION), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdt::{byte_sum, SDT_HEADER_LEN};

    #[test]
    fn xsdt_lists_64_bit_pointers() {
        let bytes = xsdt(&[0x0910_0080, 0x1_0000_0000]).expect("xsdt");
        assert_eq!(bytes.len(), xsdt_len(2));
        let header = SdtHeader::parse(&bytes).expect("header");
        assert_eq!(&header.signature, b"XSDT");
        assert_eq!(header.revision, 1);
        assert_eq!(byte_sum(&bytes), 0);
        let pointers: Vec<u64> = bytes[SDT_HEADER_LEN..]
            .chunks_exact(8)
            .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        assert_eq!(pointers, [0x0910_0080, 0x1_0000_0000]);
    }

    #[test]
    fn empty_xsdt_is_just_a_header() {
        let bytes = xsdt(&[]).expect("xsdt");
        assert_eq!(bytes.len(), SDT_HEADER_LEN);
        assert_eq!(byte_sum(&bytes), 0);
    }
}
