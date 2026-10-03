//! Generic Timer Description Table.
//!
//! ACPI 6.5 §5.2.25 (revision 3, 104 bytes). The four GSIVs are the DTB
//! `timer` node's PPIs plus 16, in the binding's order: secure EL1 physical,
//! non-secure EL1 physical, EL1 virtual, non-secure EL2 physical. Flags
//! (§5.2.25, GTDT timer flags): bit 0 mode (0 = level), bit 1 polarity
//! (0 = active high), bit 2 always-on. The DTB cells say level-high (`4`),
//! and its `always-on` property sets bit 2 on all four. There is no
//! memory-mapped counter (`CntControlBase`/`CntReadBase` = all ones), no
//! platform timers, and no virtual EL2 timer (the DTB lists none).

use crate::config::{ppi_gsiv, TimerConfig};
use crate::sdt::{table, SdtHeader, SDT_HEADER_LEN};
use crate::AcpiError;

/// GTDT signature.
pub const GTDT_SIGNATURE: [u8; 4] = *b"GTDT";
/// GTDT revision for ACPI 6.5.
pub const GTDT_REVISION: u8 = 3;
/// Bytes in a revision 3 GTDT without platform timers.
pub const GTDT_LEN: usize = 104;
/// Timer flags bit 2.
pub const TIMER_ALWAYS_ON: u32 = 1 << 2;
/// "Not provided" for the counter block addresses.
pub const NO_COUNTER_BLOCK: u64 = u64::MAX;
/// Offset of the secure EL1 GSIV; the next three timers follow every 8 bytes
/// (GSIV, then flags).
pub const TIMERS_OFFSET: usize = 48;

/// The GTDT for `timer`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(ppis = ?timer.ppis))]
pub fn gtdt(timer: &TimerConfig) -> Result<Vec<u8>, AcpiError> {
    if let Some(ppi) = timer.ppis.iter().find(|ppi| **ppi > 15) {
        return Err(AcpiError::BadConfig {
            reason: format!("timer PPI {ppi} is not 0..=15"),
        });
    }
    let mut body = vec![0u8; GTDT_LEN - SDT_HEADER_LEN];
    let at = |offset: usize| offset - SDT_HEADER_LEN;
    body[at(36)..at(44)].copy_from_slice(&NO_COUNTER_BLOCK.to_le_bytes());
    let flags = if timer.always_on { TIMER_ALWAYS_ON } else { 0 };
    for (slot, ppi) in timer.ppis.iter().enumerate() {
        let offset = at(TIMERS_OFFSET + 8 * slot);
        body[offset..offset + 4].copy_from_slice(&ppi_gsiv(*ppi).to_le_bytes());
        body[offset + 4..offset + 8].copy_from_slice(&flags.to_le_bytes());
    }
    body[at(80)..at(88)].copy_from_slice(&NO_COUNTER_BLOCK.to_le_bytes());
    tracing::debug!(
        target: "ternvale::acpi",
        gsivs = ?timer.ppis.map(ppi_gsiv),
        flags = %format!("{flags:#x}"),
        "gtdt timers"
    );
    table(SdtHeader::new(GTDT_SIGNATURE, GTDT_REVISION), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::sample;
    use crate::sdt::byte_sum;

    #[test]
    fn gtdt_matches_the_dtb_timer_cells() {
        let bytes = gtdt(&sample(1).timer).expect("gtdt");
        assert_eq!(bytes.len(), 104);
        assert_eq!(bytes[8], 3);
        assert_eq!(byte_sum(&bytes), 0);
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!([word(48), word(56), word(64), word(72)], [29, 30, 27, 26]);
        assert_eq!([word(52), word(60), word(68), word(76)], [4; 4]);
        assert_eq!(&bytes[36..44], &[0xff; 8]);
        assert_eq!(&bytes[80..88], &[0xff; 8]);
        assert_eq!(
            &bytes[88..104],
            &[0; 16],
            "no platform or EL2 virtual timers"
        );
    }

    #[test]
    fn always_on_follows_the_config_and_bad_ppis_fail() {
        let mut timer = sample(1).timer;
        timer.always_on = false;
        let bytes = gtdt(&timer).expect("gtdt");
        assert_eq!(&bytes[52..56], &[0; 4]);
        timer.ppis[0] = 16;
        assert!(gtdt(&timer).is_err());
    }
}
