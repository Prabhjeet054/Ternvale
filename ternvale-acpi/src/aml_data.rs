//! AML data objects: integers, `EisaId`, `Package`, and `Buffer`.
//!
//! ACPI 6.5 §20.2.3 (Data Objects Encoding) and §20.2.5.4:
//!
//! - Integers use `ZeroOp`/`OneOp` for 0 and 1, otherwise the smallest of
//!   `BytePrefix`, `WordPrefix`, `DWordPrefix`, `QWordPrefix`. That is the
//!   form iasl emits, so its disassembly recompiles to the same bytes.
//! - `EisaId ("PNP0A08")` (§19.6.35) is a 32-bit integer: three letters
//!   compressed to 5 bits each, then four hex digits, stored so the bytes in
//!   memory read `41 D0 0A 08`.
//! - DefPackage: `PackageOp PkgLength NumElements PackageElementList`, with
//!   NumElements one byte (at most 255 elements).
//! - DefBuffer: `BufferOp PkgLength BufferSize ByteList`, BufferSize an
//!   integer.

use crate::aml::pkg_length;
use crate::AcpiError;

/// `ZeroOp`.
pub const ZERO_OP: u8 = 0x00;
/// `OneOp`.
pub const ONE_OP: u8 = 0x01;
/// `BytePrefix`.
pub const BYTE_PREFIX: u8 = 0x0a;
/// `WordPrefix`.
pub const WORD_PREFIX: u8 = 0x0b;
/// `DWordPrefix`.
pub const DWORD_PREFIX: u8 = 0x0c;
/// `QWordPrefix`.
pub const QWORD_PREFIX: u8 = 0x0e;
/// `BufferOp`.
pub const BUFFER_OP: u8 = 0x11;
/// `PackageOp`.
pub const PACKAGE_OP: u8 = 0x12;

/// `value` as the shortest AML integer.
#[tracing::instrument(level = "trace", target = "ternvale::acpi", skip_all, fields(value))]
pub fn integer(value: u64) -> Vec<u8> {
    match value {
        0 => vec![ZERO_OP],
        1 => vec![ONE_OP],
        2..=0xff => vec![BYTE_PREFIX, value as u8],
        0x100..=0xffff => prefixed(WORD_PREFIX, &(value as u16).to_le_bytes()),
        0x1_0000..=0xffff_ffff => prefixed(DWORD_PREFIX, &(value as u32).to_le_bytes()),
        _ => prefixed(QWORD_PREFIX, &value.to_le_bytes()),
    }
}

fn prefixed(prefix: u8, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + bytes.len());
    out.push(prefix);
    out.extend_from_slice(bytes);
    out
}

/// The 32-bit value of `EisaId (id)`, where `id` is three uppercase letters
/// and four uppercase hex digits (`PNP0A08`).
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(id = %id))]
pub fn eisa_id_value(id: &str) -> Result<u32, AcpiError> {
    let bytes = id.as_bytes();
    let hex = |byte: u8| match byte {
        b'0'..=b'9' => Some(u32::from(byte - b'0')),
        b'A'..=b'F' => Some(u32::from(byte - b'A' + 10)),
        _ => None,
    };
    let letters_ok = bytes.len() == 7 && bytes[..3].iter().all(u8::is_ascii_uppercase);
    let digits: Option<Vec<u32>> = bytes
        .get(3..)
        .and_then(|d| d.iter().map(|b| hex(*b)).collect());
    let (true, Some(digits)) = (letters_ok, digits) else {
        tracing::warn!(target: "ternvale::acpi", id = %id, "invalid eisa id");
        return Err(AcpiError::BadConfig {
            reason: format!("EisaId {id:?} is not three letters A-Z and four hex digits"),
        });
    };
    let letter = |index: usize| u32::from(bytes[index] - 0x40) & 0x1f;
    let vendor = (letter(0) << 10) | (letter(1) << 5) | letter(2);
    let product = (digits[0] << 12) | (digits[1] << 8) | (digits[2] << 4) | digits[3];
    // Big-endian in memory: vendor high byte first, product last.
    Ok(((vendor << 16) | product).swap_bytes())
}

/// `EisaId (id)` as an AML integer.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(id = %id))]
pub fn eisa_id(id: &str) -> Result<Vec<u8>, AcpiError> {
    Ok(integer(u64::from(eisa_id_value(id)?)))
}

/// `Package () { elements }`, each element already encoded.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(elements = elements.len()))]
pub fn package(elements: &[Vec<u8>]) -> Result<Vec<u8>, AcpiError> {
    let Ok(count) = u8::try_from(elements.len()) else {
        tracing::warn!(target: "ternvale::acpi", elements = elements.len(), "aml package has more than 255 elements");
        return Err(AcpiError::BadConfig {
            reason: format!("package of {} elements (at most 255)", elements.len()),
        });
    };
    let content: usize = 1 + elements.iter().map(Vec::len).sum::<usize>();
    let length = pkg_length(content)?;
    let mut out = Vec::with_capacity(1 + length.len() + content);
    out.push(PACKAGE_OP);
    out.extend_from_slice(&length);
    out.push(count);
    elements.iter().for_each(|e| out.extend_from_slice(e));
    Ok(out)
}

/// `Buffer () { bytes }`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
pub fn buffer(bytes: &[u8]) -> Result<Vec<u8>, AcpiError> {
    let size = integer(bytes.len() as u64);
    let length = pkg_length(size.len() + bytes.len())?;
    let mut out = Vec::with_capacity(1 + length.len() + size.len() + bytes.len());
    out.push(BUFFER_OP);
    out.extend_from_slice(&length);
    out.extend_from_slice(&size);
    out.extend_from_slice(bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_take_the_shortest_encoding() {
        assert_eq!(integer(0), [ZERO_OP]);
        assert_eq!(integer(1), [ONE_OP]);
        assert_eq!(integer(2), [0x0a, 0x02]);
        assert_eq!(integer(0xff), [0x0a, 0xff]);
        assert_eq!(integer(0x100), [0x0b, 0x00, 0x01]);
        assert_eq!(integer(0xffff), [0x0b, 0xff, 0xff]);
        assert_eq!(integer(0x1_ffff), [0x0c, 0xff, 0xff, 0x01, 0x00]);
        assert_eq!(integer(0x1_0000_0000), [0x0e, 0, 0, 0, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn eisa_ids_match_the_well_known_values() {
        assert_eq!(eisa_id_value("PNP0A08").unwrap(), 0x080a_d041);
        assert_eq!(eisa_id_value("PNP0A03").unwrap(), 0x030a_d041);
        assert_eq!(eisa_id_value("PNP0C02").unwrap(), 0x020c_d041);
        assert_eq!(eisa_id("PNP0A08").unwrap(), [0x0c, 0x41, 0xd0, 0x0a, 0x08]);
        for bad in ["PNP0A0", "pnp0a08", "PNP0A0G", "PN10A08", "PNP0A080"] {
            assert!(eisa_id_value(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn package_and_buffer_carry_counts_and_lengths() {
        assert_eq!(
            package(&[integer(0), integer(5)]).unwrap(),
            [0x12, 0x05, 0x02, 0x00, 0x0a, 0x05]
        );
        assert!(package(&vec![integer(0); 256]).is_err());
        let big = package(&vec![integer(0); 255]).unwrap();
        // 256 content bytes + 2 PkgLength bytes = 258 = 0x102: lead 0x40 | 0x2, then 0x10.
        assert_eq!(&big[..4], &[0x12, 0x42, 0x10, 0xff], "two-byte PkgLength");
        assert_eq!(
            buffer(&[0x79, 0x00]).unwrap(),
            [0x11, 0x05, 0x0a, 0x02, 0x79, 0x00]
        );
    }
}
