//! Differentiated System Description Table.
//!
//! ACPI 6.5 §5.2.11.1: an SDT header (signature `DSDT`) followed by an AML
//! TermList. Header revision 2 or more makes AML integers 64-bit (§5.2.11.1;
//! the ASL `DefinitionBlock` ComplianceRevision, §19.6). The body is
//! `Scope (\_SB) { Device (PCI0) { ... } Device (TPM2) { ... } }`: the system
//! bus namespace (§5.3.1, Predefined Root Namespaces) holding the PCIe root
//! bridge ([`crate::pci_root`]) and, unless the VM has no TPM, the TPM 2.0
//! CRB device ([`crate::tpm2::tpm_device`]). PCI functions are found by
//! enumeration, not listed.

use crate::aml::scope;
use crate::config::AcpiConfig;
use crate::pci_root::pci_root;
use crate::sdt::{table, SdtHeader};
use crate::tpm2::tpm_device;
use crate::xhci::xhci_device;
use crate::AcpiError;

/// DSDT signature.
pub const DSDT_SIGNATURE: [u8; 4] = *b"DSDT";
/// DSDT revision: 2 selects 64-bit AML integers.
pub const DSDT_REVISION: u8 = 2;

/// The DSDT for `config`: `\_SB` with the PCIe root bridge and, when the VM
/// has one, the TPM.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(tpm = config.tpm.is_some()))]
pub fn dsdt(config: &AcpiConfig) -> Result<Vec<u8>, AcpiError> {
    let mut devices = pci_root(config)?;
    if let Some(usb) = &config.usb {
        devices.extend(xhci_device(usb)?);
    }
    if let Some(tpm) = &config.tpm {
        devices.extend(tpm_device(tpm)?);
    }
    let body = scope("\\_SB", &devices)?;
    table(SdtHeader::new(DSDT_SIGNATURE, DSDT_REVISION), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::sample;
    use crate::sdt::{byte_sum, SDT_HEADER_LEN};

    #[test]
    fn dsdt_is_header_plus_sb_scope_holding_pci0_and_tpm2() {
        let config = sample(1);
        let bytes = dsdt(&config).expect("dsdt");
        let header = SdtHeader::parse(&bytes).expect("header");
        assert_eq!(&header.signature, b"DSDT");
        assert_eq!(header.revision, 2);
        assert_eq!(header.length as usize, bytes.len());
        assert_eq!(byte_sum(&bytes), 0);
        let body = &bytes[SDT_HEADER_LEN..];
        let mut devices = pci_root(&config).expect("pci0");
        if let Some(usb) = &config.usb {
            devices.extend(xhci_device(usb).expect("xhc0"));
        }
        devices.extend(tpm_device(config.tpm.as_ref().expect("tpm")).expect("tpm2"));
        assert_eq!(body, scope("\\_SB", &devices).expect("scope").as_slice());
        assert_eq!(body[0], crate::aml::SCOPE_OP);
        let name_end = body.len() - devices.len();
        assert_eq!(&body[name_end - 5..name_end], b"\\_SB_", "RootChar + _SB_");
    }

    #[test]
    fn without_a_tpm_the_sb_scope_holds_only_pci0() {
        let config = crate::config::tests::sample_without_tpm(1);
        let bytes = dsdt(&config).expect("dsdt");
        let pci0 = pci_root(&config).expect("pci0");
        assert_eq!(
            &bytes[SDT_HEADER_LEN..],
            scope("\\_SB", &pci0).expect("scope").as_slice()
        );
        assert!(!bytes.windows(4).any(|w| w == b"TPM2"));
    }
}
