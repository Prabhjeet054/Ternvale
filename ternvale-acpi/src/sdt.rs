//! System Description Table header and checksum.
//!
//! ACPI 6.5 §5.2.6 (Table 5.4): every table except the RSDP and FACS starts
//! with this 36-byte header. All multi-byte fields are little-endian (§5.2.1).
//! The checksum byte makes the sum of every byte in the table, header
//! included, zero modulo 256.

use crate::AcpiError;

/// Bytes in a System Description Table header.
pub const SDT_HEADER_LEN: usize = 36;
/// Byte offset of the checksum inside the header.
pub const SDT_CHECKSUM_OFFSET: usize = 9;
/// OEMID written in every Ternvale table (6 bytes).
pub const OEM_ID: [u8; 6] = *b"TERNVL";
/// OEM revision written in every Ternvale table.
pub const OEM_REVISION: u32 = 1;
/// Creator ID ("vendor ID of the utility that created the table").
pub const CREATOR_ID: [u8; 4] = *b"TNVL";
/// Creator revision.
pub const CREATOR_REVISION: u32 = 1;

/// ACPI 6.5 Table 5.4, in field order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdtHeader {
    /// Table signature, for example `FACP` or `DSDT`.
    pub signature: [u8; 4],
    /// Whole table length in bytes, header included.
    pub length: u32,
    /// Table-specific revision.
    pub revision: u8,
    /// Byte that makes the table sum to zero.
    pub checksum: u8,
    /// OEM identifier.
    pub oem_id: [u8; 6],
    /// OEM table identifier.
    pub oem_table_id: [u8; 8],
    /// OEM revision of this table.
    pub oem_revision: u32,
    /// Vendor of the table builder.
    pub creator_id: [u8; 4],
    /// Revision of the table builder.
    pub creator_revision: u32,
}

impl SdtHeader {
    /// A Ternvale header with zero length and checksum; [`table`] fills both.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::acpi",
        skip_all,
        fields(signature = %String::from_utf8_lossy(&signature), revision)
    )]
    pub fn new(signature: [u8; 4], revision: u8) -> Self {
        let mut oem_table_id = *b"TERNVALE";
        oem_table_id[4..].copy_from_slice(&signature);
        Self {
            signature,
            length: 0,
            revision,
            checksum: 0,
            oem_id: OEM_ID,
            oem_table_id,
            oem_revision: OEM_REVISION,
            creator_id: CREATOR_ID,
            creator_revision: CREATOR_REVISION,
        }
    }

    /// The 36 header bytes.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
    pub fn encode(&self) -> [u8; SDT_HEADER_LEN] {
        let mut out = [0u8; SDT_HEADER_LEN];
        out[0..4].copy_from_slice(&self.signature);
        out[4..8].copy_from_slice(&self.length.to_le_bytes());
        out[8] = self.revision;
        out[SDT_CHECKSUM_OFFSET] = self.checksum;
        out[10..16].copy_from_slice(&self.oem_id);
        out[16..24].copy_from_slice(&self.oem_table_id);
        out[24..28].copy_from_slice(&self.oem_revision.to_le_bytes());
        out[28..32].copy_from_slice(&self.creator_id);
        out[32..36].copy_from_slice(&self.creator_revision.to_le_bytes());
        out
    }

    /// Read a header from the first 36 bytes of `bytes`.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
    pub fn parse(bytes: &[u8]) -> Result<Self, AcpiError> {
        let Some(raw) = bytes.get(..SDT_HEADER_LEN) else {
            tracing::warn!(target: "ternvale::acpi", len = bytes.len(), "sdt header truncated");
            return Err(AcpiError::Truncated {
                what: "sdt header",
                len: bytes.len(),
                need: SDT_HEADER_LEN,
            });
        };
        let u32_at =
            |at: usize| u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
        let mut header = Self::new([raw[0], raw[1], raw[2], raw[3]], raw[8]);
        header.length = u32_at(4);
        header.checksum = raw[SDT_CHECKSUM_OFFSET];
        header.oem_id.copy_from_slice(&raw[10..16]);
        header.oem_table_id.copy_from_slice(&raw[16..24]);
        header.oem_revision = u32_at(24);
        header.creator_id.copy_from_slice(&raw[28..32]);
        header.creator_revision = u32_at(32);
        Ok(header)
    }
}

/// Sum of `bytes` modulo 256.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
pub fn byte_sum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

/// The checksum byte for `bytes` whose checksum field is still zero: adding
/// it makes the sum zero modulo 256.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
pub fn checksum(bytes: &[u8]) -> u8 {
    0u8.wrapping_sub(byte_sum(bytes))
}

/// A whole table: `header` with its length and checksum filled, then `body`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(signature = %String::from_utf8_lossy(&header.signature), body = body.len())
)]
pub fn table(header: SdtHeader, body: &[u8]) -> Result<Vec<u8>, AcpiError> {
    let len = SDT_HEADER_LEN + body.len();
    let Ok(length) = u32::try_from(len) else {
        let error = AcpiError::TableTooLarge {
            signature: String::from_utf8_lossy(&header.signature).into_owned(),
            len,
        };
        tracing::warn!(target: "ternvale::acpi", error = %error, "acpi table rejected");
        return Err(error);
    };
    let header = SdtHeader {
        length,
        checksum: 0,
        ..header
    };
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&header.encode());
    out.extend_from_slice(body);
    out[SDT_CHECKSUM_OFFSET] = checksum(&out);
    tracing::debug!(
        target: "ternvale::acpi",
        signature = %String::from_utf8_lossy(&header.signature),
        length,
        checksum = %format!("{:#04x}", out[SDT_CHECKSUM_OFFSET]),
        "acpi table built"
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips_and_lays_out_table_5_4() {
        let mut header = SdtHeader::new(*b"DSDT", 2);
        header.length = 0x1234;
        header.checksum = 0xab;
        let bytes = header.encode();
        assert_eq!(&bytes[0..4], b"DSDT");
        assert_eq!(&bytes[4..8], &[0x34, 0x12, 0, 0]);
        assert_eq!(bytes[8], 2);
        assert_eq!(bytes[9], 0xab);
        assert_eq!(&bytes[10..16], b"TERNVL");
        assert_eq!(&bytes[16..24], b"TERNDSDT");
        assert_eq!(&bytes[28..32], b"TNVL");
        assert_eq!(SdtHeader::parse(&bytes), Ok(header));
    }

    #[test]
    fn checksum_makes_the_sum_zero() {
        assert_eq!(checksum(&[]), 0);
        assert_eq!(checksum(&[1, 2, 3]), 0xfa);
        assert_eq!(byte_sum(&[0xff, 0xff, 0x02]), 0);
        let bytes = [0x80u8; 7];
        assert_eq!(byte_sum(&bytes).wrapping_add(checksum(&bytes)), 0);
    }

    #[test]
    fn table_fills_length_and_checksum() {
        let table = table(SdtHeader::new(*b"TEST", 1), &[9, 8, 7]).expect("table");
        assert_eq!(table.len(), SDT_HEADER_LEN + 3);
        let header = SdtHeader::parse(&table).expect("parse");
        assert_eq!(header.length as usize, table.len());
        assert_eq!(byte_sum(&table), 0);
        assert_eq!(&table[SDT_HEADER_LEN..], &[9, 8, 7]);
    }

    #[test]
    fn short_header_is_rejected() {
        let error = SdtHeader::parse(&[0; 35]).unwrap_err();
        assert!(
            matches!(
                error,
                AcpiError::Truncated {
                    need: 36,
                    len: 35,
                    ..
                }
            ),
            "{error}"
        );
    }
}
