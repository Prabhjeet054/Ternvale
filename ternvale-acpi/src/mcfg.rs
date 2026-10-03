//! PCI Express memory-mapped configuration space table.
//!
//! PCI Firmware Specification 3.3 §4.1.2 (MCFG), listed by ACPI 6.5 Table 5.6:
//! signature `MCFG`, revision 1, 8 reserved bytes, then one 16-byte allocation
//! per ECAM window: base address (u64, the ECAM address of bus 0 of the
//! segment), PCI segment group (u16), start bus (u8), end bus (u8), 4 reserved.
//! Ternvale has one window on segment 0, the DTB `pcie` node's `reg` and
//! `bus-range`.

use crate::config::EcamConfig;
use crate::sdt::{table, SdtHeader};
use crate::AcpiError;

/// MCFG signature.
pub const MCFG_SIGNATURE: [u8; 4] = *b"MCFG";
/// MCFG revision.
pub const MCFG_REVISION: u8 = 1;
/// Bytes per allocation structure.
pub const MCFG_ENTRY_LEN: usize = 16;
/// Reserved bytes before the first allocation.
pub const MCFG_FIXED_LEN: usize = 8;

/// The MCFG for `ecam`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(base = %format!("{:#x}", ecam.base)))]
pub fn mcfg(ecam: &EcamConfig) -> Result<Vec<u8>, AcpiError> {
    if ecam.end_bus < ecam.start_bus {
        return Err(AcpiError::BadConfig {
            reason: format!("ecam buses {}..={}", ecam.start_bus, ecam.end_bus),
        });
    }
    // The allocation base is the address of bus 0, even when start_bus > 0.
    let bus0 = ecam
        .base
        .checked_sub(u64::from(ecam.start_bus) << 20)
        .ok_or_else(|| AcpiError::BadConfig {
            reason: format!("ecam base {:#x} is below bus {}", ecam.base, ecam.start_bus),
        })?;
    let mut body = vec![0u8; MCFG_FIXED_LEN];
    body.extend_from_slice(&bus0.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body.push(ecam.start_bus);
    body.push(ecam.end_bus);
    body.extend_from_slice(&[0; 4]);
    tracing::debug!(
        target: "ternvale::acpi",
        base = %format!("{bus0:#x}"),
        start_bus = ecam.start_bus,
        end_bus = ecam.end_bus,
        "mcfg allocation"
    );
    table(SdtHeader::new(MCFG_SIGNATURE, MCFG_REVISION), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::sample;
    use crate::sdt::byte_sum;

    #[test]
    fn mcfg_has_one_allocation_on_segment_0() {
        let bytes = mcfg(&sample(1).ecam).expect("mcfg");
        assert_eq!(bytes.len(), 36 + 8 + 16);
        assert_eq!(bytes[8], 1);
        assert_eq!(byte_sum(&bytes), 0);
        assert_eq!(&bytes[36..44], &[0; 8]);
        assert_eq!(
            u64::from_le_bytes(bytes[44..52].try_into().unwrap()),
            0x3f00_0000
        );
        assert_eq!(&bytes[52..54], &[0, 0], "segment");
        assert_eq!((bytes[54], bytes[55]), (0, 15));
    }

    #[test]
    fn base_is_bus_0_and_bad_ranges_fail() {
        let mut ecam = sample(1).ecam;
        ecam.start_bus = 2;
        let bytes = mcfg(&ecam).expect("mcfg");
        assert_eq!(
            u64::from_le_bytes(bytes[44..52].try_into().unwrap()),
            0x3ee0_0000
        );
        ecam.base = 0x10_0000;
        assert!(mcfg(&ecam).is_err());
        ecam.end_bus = 1;
        assert!(mcfg(&ecam).is_err());
    }
}
