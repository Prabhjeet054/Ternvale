//! Differentiated System Description Table.
//!
//! ACPI 6.5 §5.2.11.1: an SDT header (signature `DSDT`) followed by an AML
//! TermList. Header revision 2 or more makes AML integers 64-bit (§5.2.11.1;
//! the ASL `DefinitionBlock` ComplianceRevision, §19.6). The body is
//! `Scope (\_SB) {}`: the system bus namespace (§5.3.1, Predefined Root
//! Namespaces) with no devices yet.

use crate::aml::scope;
use crate::sdt::{table, SdtHeader};
use crate::AcpiError;

/// DSDT signature.
pub const DSDT_SIGNATURE: [u8; 4] = *b"DSDT";
/// DSDT revision: 2 selects 64-bit AML integers.
pub const DSDT_REVISION: u8 = 2;

/// The DSDT with an empty `\_SB` scope.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
pub fn dsdt() -> Result<Vec<u8>, AcpiError> {
    let body = scope("\\_SB", &[])?;
    table(SdtHeader::new(DSDT_SIGNATURE, DSDT_REVISION), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdt::{byte_sum, SDT_HEADER_LEN};

    #[test]
    fn dsdt_is_header_plus_sb_scope() {
        let bytes = dsdt().expect("dsdt");
        let header = SdtHeader::parse(&bytes).expect("header");
        assert_eq!(&header.signature, b"DSDT");
        assert_eq!(header.revision, 2);
        assert_eq!(header.length as usize, bytes.len());
        assert_eq!(byte_sum(&bytes), 0);
        assert_eq!(
            &bytes[SDT_HEADER_LEN..],
            &[0x10, 0x06, b'\\', b'_', b'S', b'B', b'_']
        );
    }
}
