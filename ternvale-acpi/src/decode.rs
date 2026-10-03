//! Bounded parsers for the tables Ternvale emits, used to check what landed in
//! guest memory. Every offset is checked against the header length and the
//! buffer; a malformed table is an error, never a panic. Layouts are the ones
//! cited in `madt.rs`, `gtdt.rs`, `mcfg.rs`, `spcr.rs`, and `dbg2.rs`.

use crate::gas::{Gas, GAS_LEN};
use crate::madt::{gicc, GICC_TYPE, GICD_TYPE, GICR_TYPE, MADT_FIXED_LEN};
use crate::mcfg::{MCFG_ENTRY_LEN, MCFG_FIXED_LEN};
use crate::sdt::{SdtHeader, SDT_HEADER_LEN};
use crate::AcpiError;

/// Most interrupt controller structures or debug devices decoded per table.
const MAX_ENTRIES: usize = 4096;

/// One GICC structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GiccInfo {
    /// ACPI Processor UID.
    pub uid: u32,
    /// GICC flags.
    pub flags: u32,
    /// GICR base (0 when the GICR structure describes the redistributors).
    pub gicr_base: u64,
    /// MPIDR.
    pub mpidr: u64,
}

/// The MADT structures Ternvale emits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MadtInfo {
    /// GICC structures in table order.
    pub giccs: Vec<GiccInfo>,
    /// (base, GIC version) of each GICD.
    pub gicds: Vec<(u64, u8)>,
    /// (base, length) of each GICR discovery range.
    pub gicrs: Vec<(u64, u32)>,
}

/// The four GTDT timers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GtdtInfo {
    /// Secure EL1, non-secure EL1, virtual EL1, EL2 GSIVs.
    pub gsivs: [u32; 4],
    /// Their flags.
    pub flags: [u32; 4],
}

/// One MCFG allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McfgAllocation {
    /// ECAM address of bus 0.
    pub base: u64,
    /// PCI segment group.
    pub segment: u16,
    /// First bus.
    pub start_bus: u8,
    /// Last bus.
    pub end_bus: u8,
}

/// The SPCR fields Ternvale sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpcrInfo {
    /// Interface Type.
    pub interface_type: u8,
    /// Base address register.
    pub base: Gas,
    /// Interrupt Type bits.
    pub interrupt_type: u8,
    /// Global System Interrupt.
    pub gsiv: u32,
}

/// One DBG2 debug device with its first register block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dbg2Device {
    /// Port Type.
    pub port_type: u16,
    /// Port Subtype.
    pub subtype: u16,
    /// First base address register.
    pub base: Gas,
    /// Its address size.
    pub size: u32,
}

/// `bytes` cut to its header length, after checking the signature.
fn body<'a>(bytes: &'a [u8], signature: &'static str) -> Result<&'a [u8], AcpiError> {
    let header = SdtHeader::parse(bytes)?;
    let bad = |reason: String| {
        tracing::warn!(target: "ternvale::acpi", signature, reason = %reason, "acpi table malformed");
        AcpiError::BadTable {
            what: signature.to_string(),
            gpa: 0,
            reason,
        }
    };
    if header.signature != signature.as_bytes() {
        return Err(bad(format!(
            "signature {:?}",
            String::from_utf8_lossy(&header.signature)
        )));
    }
    let len = header.length as usize;
    if len < SDT_HEADER_LEN || len > bytes.len() {
        return Err(bad(format!("length {len} with {} bytes", bytes.len())));
    }
    Ok(&bytes[..len])
}

/// `N` bytes at `at`, or a truncation error naming `what`.
fn field<const N: usize>(
    bytes: &[u8],
    at: usize,
    what: &'static str,
) -> Result<[u8; N], AcpiError> {
    at.checked_add(N)
        .and_then(|end| bytes.get(at..end))
        .and_then(|raw| raw.try_into().ok())
        .ok_or(AcpiError::Truncated {
            what,
            len: bytes.len(),
            need: at.saturating_add(N),
        })
}

fn u16_at(bytes: &[u8], at: usize, what: &'static str) -> Result<u16, AcpiError> {
    field(bytes, at, what).map(u16::from_le_bytes)
}

fn u32_at(bytes: &[u8], at: usize, what: &'static str) -> Result<u32, AcpiError> {
    field(bytes, at, what).map(u32::from_le_bytes)
}

fn u64_at(bytes: &[u8], at: usize, what: &'static str) -> Result<u64, AcpiError> {
    field(bytes, at, what).map(u64::from_le_bytes)
}

/// Decode an `APIC` table.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
pub fn decode_madt(bytes: &[u8]) -> Result<MadtInfo, AcpiError> {
    let table = body(bytes, "APIC")?;
    let mut info = MadtInfo::default();
    let mut at = SDT_HEADER_LEN + MADT_FIXED_LEN;
    let mut seen = 0;
    while at < table.len() {
        let [kind, len] = field::<2>(table, at, "madt structure header")?;
        let len = usize::from(len);
        let end = at
            .checked_add(len)
            .filter(|end| len >= 2 && *end <= table.len());
        let Some(end) = end else {
            tracing::warn!(target: "ternvale::acpi", at, len, "madt structure overruns the table");
            return Err(AcpiError::Truncated {
                what: "madt structure",
                len: table.len(),
                need: at.saturating_add(len.max(2)),
            });
        };
        let s = &table[at..end];
        match kind {
            GICC_TYPE => info.giccs.push(GiccInfo {
                uid: u32_at(s, gicc::UID, "gicc uid")?,
                flags: u32_at(s, gicc::FLAGS, "gicc flags")?,
                gicr_base: u64_at(s, gicc::GICR_BASE, "gicc gicr base")?,
                mpidr: u64_at(s, gicc::MPIDR, "gicc mpidr")?,
            }),
            GICD_TYPE => info.gicds.push((
                u64_at(s, 8, "gicd base")?,
                field::<1>(s, 20, "gicd version")?[0],
            )),
            GICR_TYPE => info
                .gicrs
                .push((u64_at(s, 4, "gicr base")?, u32_at(s, 12, "gicr length")?)),
            _ => {}
        }
        seen += 1;
        if seen > MAX_ENTRIES {
            return Err(AcpiError::Truncated {
                what: "madt structure count",
                len: seen,
                need: MAX_ENTRIES,
            });
        }
        at = end;
    }
    Ok(info)
}

/// Decode a `GTDT`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
pub fn decode_gtdt(bytes: &[u8]) -> Result<GtdtInfo, AcpiError> {
    let table = body(bytes, "GTDT")?;
    let mut gsivs = [0; 4];
    let mut flags = [0; 4];
    for slot in 0..4 {
        gsivs[slot] = u32_at(table, 48 + 8 * slot, "gtdt gsiv")?;
        flags[slot] = u32_at(table, 52 + 8 * slot, "gtdt flags")?;
    }
    Ok(GtdtInfo { gsivs, flags })
}

/// Decode an `MCFG`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
pub fn decode_mcfg(bytes: &[u8]) -> Result<Vec<McfgAllocation>, AcpiError> {
    let table = body(bytes, "MCFG")?;
    let start = SDT_HEADER_LEN + MCFG_FIXED_LEN;
    let entries = table.len().saturating_sub(start) / MCFG_ENTRY_LEN;
    (0..entries.min(MAX_ENTRIES))
        .map(|index| {
            let at = start + index * MCFG_ENTRY_LEN;
            let [start_bus, end_bus] = field::<2>(table, at + 10, "mcfg buses")?;
            Ok(McfgAllocation {
                base: u64_at(table, at, "mcfg base")?,
                segment: u16_at(table, at + 8, "mcfg segment")?,
                start_bus,
                end_bus,
            })
        })
        .collect()
}

/// Decode an `SPCR`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
pub fn decode_spcr(bytes: &[u8]) -> Result<SpcrInfo, AcpiError> {
    let table = body(bytes, "SPCR")?;
    Ok(SpcrInfo {
        interface_type: field::<1>(table, 36, "spcr interface type")?[0],
        base: Gas::parse(&field::<GAS_LEN>(table, 40, "spcr base")?)?,
        interrupt_type: field::<1>(table, 52, "spcr interrupt type")?[0],
        gsiv: u32_at(table, 54, "spcr gsiv")?,
    })
}

/// Decode a `DBG2`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
pub fn decode_dbg2(bytes: &[u8]) -> Result<Vec<Dbg2Device>, AcpiError> {
    let table = body(bytes, "DBG2")?;
    let mut at = u32_at(table, 36, "dbg2 device info offset")? as usize;
    let count = u32_at(table, 40, "dbg2 device count")? as usize;
    let mut devices = Vec::new();
    for _ in 0..count.min(MAX_ENTRIES) {
        let len = usize::from(u16_at(table, at + 1, "dbg2 device length")?);
        let device = at
            .checked_add(len)
            .filter(|_| len > 0)
            .and_then(|end| table.get(at..end))
            .ok_or(AcpiError::Truncated {
                what: "dbg2 device",
                len: table.len(),
                need: at.saturating_add(len),
            })?;
        let base_at = usize::from(u16_at(device, 18, "dbg2 base offset")?);
        let size_at = usize::from(u16_at(device, 20, "dbg2 size offset")?);
        devices.push(Dbg2Device {
            port_type: u16_at(device, 12, "dbg2 port type")?,
            subtype: u16_at(device, 14, "dbg2 port subtype")?,
            base: Gas::parse(&field::<GAS_LEN>(device, base_at, "dbg2 base")?)?,
            size: u32_at(device, size_at, "dbg2 address size")?,
        });
        at += len;
    }
    Ok(devices)
}

#[cfg(test)]
#[path = "decode_tests.rs"]
mod tests;
