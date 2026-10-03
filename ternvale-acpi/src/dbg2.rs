//! Debug Port Table 2: Windows's kernel debug transport.
//!
//! Microsoft "Debug Port Table 2 (DBG2)" specification: signature `DBG2`,
//! revision 0. After the SDT header: OffsetDbgDeviceInfo (u32) and
//! NumberDbgDeviceInfo (u32), then one Debug Device Information structure:
//!
//! | Offset | Field | Value |
//! | --- | --- | --- |
//! | 0 | Revision | 0 |
//! | 1 | Length | whole structure, namespace string included |
//! | 3 | NumberofGenericAddressRegisters | 1 |
//! | 4, 6 | NamespaceString length, offset | 2 (`"."` + NUL), 38 |
//! | 8, 10 | OemData length, offset | 0, 0 |
//! | 12 | Port Type | 0x8000, serial |
//! | 14 | Port Subtype | 0x0003, ARM PL011 UART |
//! | 18, 20 | BaseAddressRegister offset, AddressSize offset | 22, 34 |
//! | 22 | BaseAddressRegister | GAS for `UART_BASE` |
//! | 34 | AddressSize | the PL011 register block length |
//!
//! The namespace string is `"."`, which the spec reserves for a device not
//! described in the ACPI namespace (the DSDT has no UART device yet).
//! TODO(verify): Windows kdcom/kdnet acceptance of `"."` here; QEMU uses a
//! `\_SB.COM0` device and the string `"COM0"`.

use crate::config::UartConfig;
use crate::gas::{Gas, GAS_LEN};
use crate::sdt::{table, SdtHeader};
use crate::AcpiError;

/// DBG2 signature.
pub const DBG2_SIGNATURE: [u8; 4] = *b"DBG2";
/// DBG2 revision.
pub const DBG2_REVISION: u8 = 0;
/// Port Type: serial.
pub const PORT_SERIAL: u16 = 0x8000;
/// Port Subtype: ARM PL011 UART.
pub const SUBTYPE_PL011: u16 = 0x0003;
/// Offset of the device information structure from the table start.
pub const DEVICE_INFO_OFFSET: u32 = 44;
/// Namespace string for a device outside the ACPI namespace.
pub const NO_NAMESPACE: &[u8] = b".\0";
/// Offset of the GAS inside the device information structure.
pub const DEVICE_GAS_OFFSET: usize = 22;

/// The DBG2 for `uart`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(base = %format!("{:#x}", uart.base)))]
pub fn dbg2(uart: &UartConfig) -> Result<Vec<u8>, AcpiError> {
    let size_offset = DEVICE_GAS_OFFSET + GAS_LEN;
    let namespace_offset = size_offset + 4;
    let length = namespace_offset + NO_NAMESPACE.len();
    let mut info = Vec::with_capacity(length);
    info.push(0);
    info.extend_from_slice(&(length as u16).to_le_bytes());
    info.push(1);
    info.extend_from_slice(&(NO_NAMESPACE.len() as u16).to_le_bytes());
    info.extend_from_slice(&(namespace_offset as u16).to_le_bytes());
    info.extend_from_slice(&[0; 4]);
    info.extend_from_slice(&PORT_SERIAL.to_le_bytes());
    info.extend_from_slice(&SUBTYPE_PL011.to_le_bytes());
    info.extend_from_slice(&[0; 2]);
    info.extend_from_slice(&(DEVICE_GAS_OFFSET as u16).to_le_bytes());
    info.extend_from_slice(&(size_offset as u16).to_le_bytes());
    info.extend_from_slice(&Gas::mmio32(uart.base).encode());
    info.extend_from_slice(&uart.len.to_le_bytes());
    info.extend_from_slice(NO_NAMESPACE);
    let mut body = Vec::with_capacity(8 + info.len());
    body.extend_from_slice(&DEVICE_INFO_OFFSET.to_le_bytes());
    body.extend_from_slice(&1u32.to_le_bytes());
    body.extend_from_slice(&info);
    tracing::debug!(
        target: "ternvale::acpi",
        base = %format!("{:#x}", uart.base),
        size = %format!("{:#x}", uart.len),
        "dbg2 pl011"
    );
    table(SdtHeader::new(DBG2_SIGNATURE, DBG2_REVISION), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::sample;
    use crate::sdt::byte_sum;

    #[test]
    fn dbg2_describes_one_pl011() {
        let bytes = dbg2(&sample(1).uart).expect("dbg2");
        assert_eq!(bytes.len(), 44 + 40);
        assert_eq!(bytes[8], 0);
        assert_eq!(byte_sum(&bytes), 0);
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!((u32_at(36), u32_at(40)), (44, 1));
        let info = 44;
        assert_eq!((bytes[info], u16_at(info + 1), bytes[info + 3]), (0, 40, 1));
        assert_eq!((u16_at(info + 4), u16_at(info + 6)), (2, 38));
        assert_eq!((u16_at(info + 12), u16_at(info + 14)), (0x8000, 3));
        assert_eq!((u16_at(info + 18), u16_at(info + 20)), (22, 34));
        assert_eq!(
            Gas::parse(&bytes[info + 22..]),
            Ok(Gas::mmio32(0x0900_0000))
        );
        assert_eq!(u32_at(info + 34), 0x1000);
        assert_eq!(&bytes[info + 38..], b".\0");
    }
}
