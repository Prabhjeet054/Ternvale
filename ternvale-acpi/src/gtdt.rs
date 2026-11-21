//! Generic Timer Description Table.
//!
//! ACPI 6.5 §5.2.24 (GTDT revision 3, 104-byte fixed header). The four GSIVs
//! are the DTB `timer` node's PPIs plus 16. Optional Type 1 Generic Watchdog
//! structure (§5.2.24.3, length 28): refresh/control frames and one GSIV
//! (Linux `sbsa_gwdt` / `arm,sbsa-gwdt`).

use crate::config::{ppi_gsiv, spi_gsiv, TimerConfig, WatchdogConfig};
use crate::sdt::{table, SdtHeader, SDT_HEADER_LEN};
use crate::AcpiError;

/// GTDT signature.
pub const GTDT_SIGNATURE: [u8; 4] = *b"GTDT";
/// GTDT revision for ACPI 6.5.
pub const GTDT_REVISION: u8 = 3;
/// Bytes in a revision 3 GTDT without platform timers.
pub const GTDT_LEN: usize = 104;
/// Type 1 Generic Watchdog structure length (ACPI 6.5 §5.2.24.3).
pub const GTDT_WATCHDOG_LEN: usize = 28;
/// Timer flags bit 2.
pub const TIMER_ALWAYS_ON: u32 = 1 << 2;
/// "Not provided" for the counter block addresses.
pub const NO_COUNTER_BLOCK: u64 = u64::MAX;
/// Offset of the secure EL1 GSIV; the next three timers follow every 8 bytes
/// (GSIV, then flags).
pub const TIMERS_OFFSET: usize = 48;
/// Platform Timer Count.
const PLATFORM_COUNT_OFFSET: usize = 88;
/// Platform Timer Offset.
const PLATFORM_OFFSET_OFFSET: usize = 92;
/// Generic Watchdog structure type.
pub const GTDT_WATCHDOG_TYPE: u8 = 1;

/// The GTDT for `timer` and optional SBSA watchdog.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(ppis = ?timer.ppis, watchdog = watchdog.is_some()))]
pub fn gtdt(timer: &TimerConfig, watchdog: Option<&WatchdogConfig>) -> Result<Vec<u8>, AcpiError> {
    if let Some(ppi) = timer.ppis.iter().find(|ppi| **ppi > 15) {
        return Err(AcpiError::BadConfig {
            reason: format!("timer PPI {ppi} is not 0..=15"),
        });
    }
    let extra = if watchdog.is_some() {
        GTDT_WATCHDOG_LEN
    } else {
        0
    };
    let mut body = vec![0u8; GTDT_LEN - SDT_HEADER_LEN + extra];
    let at = |offset: usize| offset - SDT_HEADER_LEN;
    body[at(36)..at(44)].copy_from_slice(&NO_COUNTER_BLOCK.to_le_bytes());
    let flags = if timer.always_on { TIMER_ALWAYS_ON } else { 0 };
    for (slot, ppi) in timer.ppis.iter().enumerate() {
        let offset = at(TIMERS_OFFSET + 8 * slot);
        body[offset..offset + 4].copy_from_slice(&ppi_gsiv(*ppi).to_le_bytes());
        body[offset + 4..offset + 8].copy_from_slice(&flags.to_le_bytes());
    }
    body[at(80)..at(88)].copy_from_slice(&NO_COUNTER_BLOCK.to_le_bytes());
    if let Some(wd) = watchdog {
        body[at(PLATFORM_COUNT_OFFSET)..at(PLATFORM_COUNT_OFFSET) + 4]
            .copy_from_slice(&1u32.to_le_bytes());
        body[at(PLATFORM_OFFSET_OFFSET)..at(PLATFORM_OFFSET_OFFSET) + 4]
            .copy_from_slice(&(GTDT_LEN as u32).to_le_bytes());
        let w = at(GTDT_LEN);
        body[w] = GTDT_WATCHDOG_TYPE;
        body[w + 1] = GTDT_WATCHDOG_LEN as u8;
        body[w + 4..w + 12].copy_from_slice(&wd.refresh_base.to_le_bytes());
        body[w + 12..w + 20].copy_from_slice(&wd.control_base.to_le_bytes());
        body[w + 20..w + 24].copy_from_slice(&spi_gsiv(wd.spi).to_le_bytes());
        // flags: level, active high, non-secure
        body[w + 24..w + 28].copy_from_slice(&0u32.to_le_bytes());
        tracing::debug!(
            target: "ternvale::acpi",
            refresh = format!("{:#x}", wd.refresh_base),
            control = format!("{:#x}", wd.control_base),
            gsiv = spi_gsiv(wd.spi),
            "gtdt SBSA watchdog"
        );
    }
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
        let bytes = gtdt(&sample(1).timer, None).expect("gtdt");
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
    fn watchdog_structure_is_type_1_length_28() {
        let wd = sample(1).watchdog.expect("wd");
        let bytes = gtdt(&sample(1).timer, Some(&wd)).expect("gtdt");
        assert_eq!(bytes.len(), 104 + 28);
        assert_eq!(byte_sum(&bytes), 0);
        assert_eq!(
            u32::from_le_bytes(bytes[88..92].try_into().unwrap()),
            1,
            "platform timer count"
        );
        assert_eq!(u32::from_le_bytes(bytes[92..96].try_into().unwrap()), 104);
        assert_eq!(bytes[104], 1);
        assert_eq!(bytes[105], 28);
        assert_eq!(&bytes[108..116], &0x0906_0000u64.to_le_bytes());
        assert_eq!(&bytes[116..124], &0x0907_0000u64.to_le_bytes());
    }

    #[test]
    fn always_on_follows_the_config_and_bad_ppis_fail() {
        let mut timer = sample(1).timer;
        timer.always_on = false;
        let bytes = gtdt(&timer, None).expect("gtdt");
        assert_eq!(&bytes[52..56], &[0; 4]);
        timer.ppis[0] = 16;
        assert!(gtdt(&timer, None).is_err());
    }
}
