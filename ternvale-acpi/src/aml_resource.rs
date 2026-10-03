//! ACPI resource descriptors for `_CRS` (ACPI 6.5 §6.4).
//!
//! - Word Address Space Descriptor (§6.4.3.5.3, tag `0x88`, 13 bytes after
//!   the length): ASL `WordBusNumber` when the resource type is 2.
//! - DWord Address Space Descriptor (§6.4.3.5.2, tag `0x87`, 23 bytes after
//!   the length): ASL `DWordMemory` when the resource type is 0.
//! - End Tag (§6.4.2.9, `0x79` + checksum). A zero checksum means "ignore";
//!   iasl writes zero too.
//!
//! Both address descriptors are emitted fixed-size and fixed-location
//! (`_MIF` = `_MAF` = 1, `_GRA` = 0, `_LEN` = `_MAX` - `_MIN` + 1, the valid
//! combination in §6.4.3.5, Table 6.44), positive decode, no translation, and
//! without the optional ResourceSource fields.

use crate::aml_data::buffer;
use crate::AcpiError;

/// Large item tag of the DWord Address Space Descriptor.
pub const DWORD_ADDRESS_TAG: u8 = 0x87;
/// Large item tag of the Word Address Space Descriptor.
pub const WORD_ADDRESS_TAG: u8 = 0x88;
/// Small item End Tag (type 0xF, length 1).
pub const END_TAG: u8 = 0x79;
/// Resource type: memory range.
pub const RESOURCE_MEMORY: u8 = 0;
/// Resource type: bus number range.
pub const RESOURCE_BUS: u8 = 2;
/// General flags: `_MAF` (bit 3) and `_MIF` (bit 2) fixed, positive decode.
const FIXED_POS_DECODE: u8 = 0b1100;
/// General flags bit 0: this device consumes the range (else it produces it
/// for its children, as a bridge window does).
pub const CONSUMER: u8 = 1;
/// Memory type-specific flags: `_RW` = 1 (read/write), `_MEM` = 0
/// (non-cacheable), `_MTP` = 0 (AddressRangeMemory), `_TTP` = 0.
pub const MEM_NONCACHEABLE_RW: u8 = 0x01;

/// Producer or consumer (general flags bit 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    /// A bridge window passed down to children (`ResourceProducer`).
    Producer,
    /// A range the device itself decodes (`ResourceConsumer`).
    Consumer,
}

impl Usage {
    fn flags(self) -> u8 {
        match self {
            Self::Producer => FIXED_POS_DECODE,
            Self::Consumer => FIXED_POS_DECODE | CONSUMER,
        }
    }
}

/// `WordBusNumber (ResourceProducer, MinFixed, MaxFixed, PosDecode, 0,
/// first, last, 0, last - first + 1)`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(first, last)
)]
pub fn word_bus_number(first: u16, last: u16) -> Result<Vec<u8>, AcpiError> {
    if last < first {
        return Err(bad(format!("bus range {first}..={last}")));
    }
    let len = u32::from(last - first) + 1;
    let Ok(len) = u16::try_from(len) else {
        return Err(bad(format!("bus range {first}..={last} is 65536 buses")));
    };
    let mut out = vec![
        WORD_ADDRESS_TAG,
        13,
        0,
        RESOURCE_BUS,
        Usage::Producer.flags(),
        0,
    ];
    for field in [0, first, last, 0, len] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    Ok(out)
}

/// `DWordMemory (usage, PosDecode, MinFixed, MaxFixed, NonCacheable,
/// ReadWrite, 0, base, base + len - 1, 0, len)`. The range must be non-empty
/// and end at or below 4 GiB.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(usage = ?usage, base = %format!("{base:#x}"), len = %format!("{len:#x}"))
)]
pub fn dword_memory(usage: Usage, base: u64, len: u64) -> Result<Vec<u8>, AcpiError> {
    let last = base.checked_add(len).and_then(|end| end.checked_sub(1));
    let (Ok(base32), Some(Ok(last32)), Ok(len32)) = (
        u32::try_from(base),
        last.map(u32::try_from),
        u32::try_from(len),
    ) else {
        return Err(bad(format!(
            "memory {base:#x}+{len:#x} does not fit a DWord descriptor"
        )));
    };
    if len == 0 {
        return Err(bad(format!("memory {base:#x} has zero length")));
    }
    let mut out = vec![
        DWORD_ADDRESS_TAG,
        23,
        0,
        RESOURCE_MEMORY,
        usage.flags(),
        MEM_NONCACHEABLE_RW,
    ];
    for field in [0, base32, last32, 0, len32] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    Ok(out)
}

/// `ResourceTemplate () { descriptors }`: a Buffer holding the descriptors
/// and an End Tag.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(descriptors = descriptors.len()))]
pub fn resource_template(descriptors: &[Vec<u8>]) -> Result<Vec<u8>, AcpiError> {
    let mut bytes: Vec<u8> = descriptors.concat();
    bytes.extend_from_slice(&[END_TAG, 0]);
    buffer(&bytes)
}

fn bad(reason: String) -> AcpiError {
    tracing::warn!(target: "ternvale::acpi", reason = %reason, "acpi resource descriptor rejected");
    AcpiError::BadConfig { reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_range_descriptor_matches_the_spec_layout() {
        assert_eq!(
            word_bus_number(0, 15).unwrap(),
            [
                0x88, 0x0d, 0x00, // tag, length 13
                0x02, 0x0c, 0x00, // bus range, producer MinFixed MaxFixed PosDecode, no flags
                0x00, 0x00, // _GRA
                0x00, 0x00, // _MIN
                0x0f, 0x00, // _MAX
                0x00, 0x00, // _TRA
                0x10, 0x00, // _LEN
            ]
        );
        assert!(word_bus_number(2, 1).is_err());
        assert!(word_bus_number(0, u16::MAX).is_err());
    }

    #[test]
    fn memory_descriptor_matches_the_spec_layout() {
        let window = dword_memory(Usage::Producer, 0x1000_0000, 0x2f00_0000).unwrap();
        assert_eq!(window.len(), 26);
        assert_eq!(&window[..6], &[0x87, 0x17, 0x00, 0x00, 0x0c, 0x01]);
        let field =
            |i: usize| u32::from_le_bytes(window[6 + 4 * i..10 + 4 * i].try_into().unwrap());
        assert_eq!(
            [field(0), field(1), field(2), field(3), field(4)],
            [0, 0x1000_0000, 0x3eff_ffff, 0, 0x2f00_0000]
        );
        let ecam = dword_memory(Usage::Consumer, 0x3f00_0000, 0x0100_0000).unwrap();
        assert_eq!(ecam[4], 0x0d, "consumer bit set");
        assert!(dword_memory(Usage::Producer, 0x1000, 0).is_err());
        assert!(dword_memory(Usage::Producer, 0xffff_f000, 0x2000).is_err());
        assert!(dword_memory(Usage::Producer, 0xffff_f000, 0x1000).is_ok());
        assert!(dword_memory(Usage::Producer, u64::MAX, 2).is_err());
    }

    #[test]
    fn template_ends_with_an_end_tag_inside_a_buffer() {
        let template = resource_template(&[word_bus_number(0, 0).unwrap()]).unwrap();
        // BufferOp, PkgLength 0x15, BufferSize 0x12, 16 descriptor bytes, 79 00.
        assert_eq!(&template[..3], &[0x11, 0x15, 0x0a]);
        assert_eq!(template[3], 18);
        assert_eq!(&template[template.len() - 2..], &[0x79, 0x00]);
    }
}
