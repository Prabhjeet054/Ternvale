//! Serial Port Console Redirection table: Windows's boot console.
//!
//! Microsoft "Serial Port Console Redirection Table" specification, revision 2
//! layout (80 bytes), as QEMU `virt` emits it:
//!
//! | Offset | Field | Value |
//! | --- | --- | --- |
//! | 36 | Interface Type | 3, ARM PL011 UART (DBG2 serial subtypes table) |
//! | 40 | Base Address | GAS: system memory, 32-bit, dword access, `UART_BASE` |
//! | 52 | Interrupt Type | bit 3, ARMH GIC interrupt |
//! | 54 | Global System Interrupt | the PL011 SPI + 32 |
//! | 58 | Configured Baud Rate | 7, 115200 |
//! | 59–63 | Parity, Stop Bits, Flow Control, Terminal Type, Language | 0, 1, 0, 0 (VT100), 0 |
//! | 64 | PCI Device ID, Vendor ID | 0xFFFF (not a PCI device) |
//! | 76 | Reserved (UART Clock Frequency from revision 3) | 0 |
//!
//! TODO(verify): SPCR revision 2 vs 3/4 for current Windows on Arm, and
//! whether Windows wants a non-zero clock. iasl 20260408 decodes every SPCR
//! with the revision 4 template and reports revision 2 tables (QEMU's too) as
//! ending mid-structure.

use crate::config::{spi_gsiv, UartConfig};
use crate::gas::Gas;
use crate::sdt::{table, SdtHeader, SDT_HEADER_LEN};
use crate::AcpiError;

/// SPCR signature.
pub const SPCR_SIGNATURE: [u8; 4] = *b"SPCR";
/// SPCR revision.
pub const SPCR_REVISION: u8 = 2;
/// Bytes in a revision 2 SPCR.
pub const SPCR_LEN: usize = 80;
/// Interface Type 3: ARM PL011 UART.
pub const INTERFACE_PL011: u8 = 3;
/// Interrupt Type bit 3: ARMH GIC interrupt.
pub const INTERRUPT_TYPE_GIC: u8 = 1 << 3;
/// Configured Baud Rate 7: 115200.
pub const BAUD_115200: u8 = 7;
/// Offset of the base address GAS.
pub const BASE_OFFSET: usize = 40;
/// Offset of the Global System Interrupt.
pub const GSIV_OFFSET: usize = 54;

/// The SPCR for `uart`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(base = %format!("{:#x}", uart.base)))]
pub fn spcr(uart: &UartConfig) -> Result<Vec<u8>, AcpiError> {
    let mut body = vec![0u8; SPCR_LEN - SDT_HEADER_LEN];
    let at = |offset: usize| offset - SDT_HEADER_LEN;
    body[at(36)] = INTERFACE_PL011;
    body[at(BASE_OFFSET)..at(BASE_OFFSET) + 12].copy_from_slice(&Gas::mmio32(uart.base).encode());
    body[at(52)] = INTERRUPT_TYPE_GIC;
    let gsiv = spi_gsiv(uart.spi);
    body[at(GSIV_OFFSET)..at(GSIV_OFFSET) + 4].copy_from_slice(&gsiv.to_le_bytes());
    body[at(58)] = BAUD_115200;
    body[at(60)] = 1;
    body[at(64)..at(68)].fill(0xff);
    tracing::debug!(target: "ternvale::acpi", base = %format!("{:#x}", uart.base), gsiv, "spcr pl011");
    table(SdtHeader::new(SPCR_SIGNATURE, SPCR_REVISION), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::sample;
    use crate::sdt::byte_sum;

    #[test]
    fn spcr_points_at_the_pl011() {
        let bytes = spcr(&sample(1).uart).expect("spcr");
        assert_eq!(bytes.len(), 80);
        assert_eq!(bytes[8], 2);
        assert_eq!(byte_sum(&bytes), 0);
        assert_eq!(bytes[36], 3);
        assert_eq!(Gas::parse(&bytes[40..52]), Ok(Gas::mmio32(0x0900_0000)));
        assert_eq!(bytes[52], 8);
        assert_eq!(u32::from_le_bytes(bytes[54..58].try_into().unwrap()), 33);
        assert_eq!(&bytes[58..64], &[7, 0, 1, 0, 0, 0]);
        assert_eq!(&bytes[64..68], &[0xff; 4]);
        assert_eq!(&bytes[68..80], &[0; 12]);
    }
}
