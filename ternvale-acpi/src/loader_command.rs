//! One QEMU linker/loader command (`QEMU_LOADER_ENTRY`, 128 bytes), as laid
//! out in EDK2 `OvmfPkg/Include/IndustryStandard/QemuLoader.h` and QEMU
//! `hw/acpi/bios-linker-loader.c`. [`crate::LoaderBlobs`] builds the script.

use crate::AcpiError;

/// Bytes per loader command (`QEMU_LOADER_ENTRY`).
pub const LOADER_ENTRY_LEN: usize = 128;
/// fw_cfg and loader file name field, NUL included (`QEMU_LOADER_FNAME_SIZE`).
pub const FNAME_LEN: usize = 56;

/// Where an `ALLOCATE` asks for memory (`QEMU_LOADER_ALLOC_ZONE`). EDK2 only
/// logs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// Anywhere (`QemuLoaderAllocHigh` = 1).
    High = 1,
    /// The legacy F segment (`QemuLoaderAllocFSeg` = 2).
    FSeg = 2,
}

/// One `QEMU_LOADER_ENTRY`. Every integer is little-endian.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoaderCommand {
    /// Type 1: download `file` into memory aligned to `align`.
    Allocate {
        /// fw_cfg file name.
        file: String,
        /// Power of two, at most 4096 (EDK2's limit).
        align: u32,
        /// Requested zone.
        zone: Zone,
    },
    /// Type 2: add `pointee`'s base to the `size`-byte field at `offset` in `pointer`.
    AddPointer {
        /// File holding the pointer field.
        pointer: String,
        /// File the pointer points into.
        pointee: String,
        /// Byte offset of the field in `pointer`.
        offset: u32,
        /// 1, 2, 4, or 8.
        size: u8,
    },
    /// Type 3: store the checksum of `start..start + len` at `result`.
    AddChecksum {
        /// File to patch.
        file: String,
        /// Byte that receives the checksum.
        result: u32,
        /// First byte summed.
        start: u32,
        /// Bytes summed.
        len: u32,
    },
}

impl LoaderCommand {
    /// The 128-byte entry.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
    pub fn encode(&self) -> Result<[u8; LOADER_ENTRY_LEN], AcpiError> {
        let mut entry = [0u8; LOADER_ENTRY_LEN];
        match self {
            Self::Allocate { file, align, zone } => {
                entry[..4].copy_from_slice(&1u32.to_le_bytes());
                put_name(&mut entry[4..4 + FNAME_LEN], file)?;
                entry[60..64].copy_from_slice(&align.to_le_bytes());
                entry[64] = *zone as u8;
            }
            Self::AddPointer {
                pointer,
                pointee,
                offset,
                size,
            } => {
                entry[..4].copy_from_slice(&2u32.to_le_bytes());
                put_name(&mut entry[4..4 + FNAME_LEN], pointer)?;
                put_name(&mut entry[60..60 + FNAME_LEN], pointee)?;
                entry[116..120].copy_from_slice(&offset.to_le_bytes());
                entry[120] = *size;
            }
            Self::AddChecksum {
                file,
                result,
                start,
                len,
            } => {
                entry[..4].copy_from_slice(&3u32.to_le_bytes());
                put_name(&mut entry[4..4 + FNAME_LEN], file)?;
                entry[60..64].copy_from_slice(&result.to_le_bytes());
                entry[64..68].copy_from_slice(&start.to_le_bytes());
                entry[68..72].copy_from_slice(&len.to_le_bytes());
            }
        }
        Ok(entry)
    }

    /// Parse one entry. Unknown types (EDK2 skips them) and malformed names
    /// are errors here.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = entry.len()))]
    pub fn decode(entry: &[u8]) -> Result<Self, AcpiError> {
        if entry.len() != LOADER_ENTRY_LEN {
            return Err(AcpiError::Truncated {
                what: "loader entry",
                len: entry.len(),
                need: LOADER_ENTRY_LEN,
            });
        }
        let u32_at = |at: usize| {
            u32::from_le_bytes([entry[at], entry[at + 1], entry[at + 2], entry[at + 3]])
        };
        Ok(match u32_at(0) {
            1 => Self::Allocate {
                file: get_name(&entry[4..4 + FNAME_LEN])?,
                align: u32_at(60),
                zone: match entry[64] {
                    1 => Zone::High,
                    2 => Zone::FSeg,
                    other => return Err(bad_loader(format!("allocate zone {other}"))),
                },
            },
            2 => Self::AddPointer {
                pointer: get_name(&entry[4..4 + FNAME_LEN])?,
                pointee: get_name(&entry[60..60 + FNAME_LEN])?,
                offset: u32_at(116),
                size: entry[120],
            },
            3 => Self::AddChecksum {
                file: get_name(&entry[4..4 + FNAME_LEN])?,
                result: u32_at(60),
                start: u32_at(64),
                len: u32_at(68),
            },
            other => return Err(bad_loader(format!("command type {other}"))),
        })
    }
}

fn put_name(field: &mut [u8], name: &str) -> Result<(), AcpiError> {
    if name.is_empty() || name.len() >= field.len() || name.bytes().any(|b| b == 0) {
        return Err(bad_loader(format!(
            "file name {name:?} does not fit {} bytes with its NUL",
            field.len()
        )));
    }
    field[..name.len()].copy_from_slice(name.as_bytes());
    Ok(())
}

fn get_name(field: &[u8]) -> Result<String, AcpiError> {
    let Some(end) = field.iter().position(|&b| b == 0) else {
        return Err(bad_loader("file name has no NUL".into()));
    };
    String::from_utf8(field[..end].to_vec())
        .map_err(|_| bad_loader("file name is not UTF-8".into()))
}

pub(crate) fn bad_loader(reason: String) -> AcpiError {
    tracing::warn!(target: "ternvale::acpi", %reason, "acpi loader script rejected");
    AcpiError::BadTable {
        what: "acpi loader".to_string(),
        gpa: 0,
        reason,
    }
}
