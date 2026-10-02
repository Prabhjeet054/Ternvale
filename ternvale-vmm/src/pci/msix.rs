//! MSI-X capability, vector table, and PBA. A logged stub: nothing is delivered.
//!
//! The model is complete enough for a driver to size, program, and enable
//! vectors, and every step is logged. [`Msix::signal`] is where delivery will
//! go: Hypervisor.framework's GIC can take MSIs (macOS 15 `hv_gic_send_msi`
//! plus an MSI doorbell region in `hv_gic_config`). TODO(verify) the exact API
//! names and whether the doorbell must be advertised as a GICv3 MBI frame
//! (`msi-controller` + `mbi-ranges`) or a GICv2m frame in the DTB. Until then
//! functions keep MSI-X off by default and interrupt through INTx.

use super::{ConfigSpace, PciError};

/// MSI-X capability ID.
pub const MSIX_CAP_ID: u8 = 0x11;
/// Bytes per vector table entry: address low, address high, data, control.
pub const MSIX_ENTRY_SIZE: u64 = 16;
/// Virtio "no vector" value, also used for unrouted vectors.
pub const MSIX_NO_VECTOR: u16 = 0xffff;

const CONTROL_ENABLE: u16 = 1 << 15;
const CONTROL_FUNCTION_MASK: u16 = 1 << 14;
const ENTRY_MASKED: u32 = 1;

/// MSI-X state for one function.
#[derive(Debug)]
pub struct Msix {
    name: String,
    bar: u8,
    table_offset: u32,
    pba_offset: u32,
    table: Vec<[u32; 4]>,
    cap: Option<u8>,
    enabled: bool,
}

impl Msix {
    /// `vectors` entries; table and PBA live in BAR `bar` at the given offsets
    /// (8-byte aligned).
    pub fn new(name: String, vectors: u16, bar: u8, table_offset: u32, pba_offset: u32) -> Self {
        let table = (0..vectors.max(1))
            .map(|_| [0, 0, 0, ENTRY_MASKED])
            .collect();
        Self {
            name,
            bar,
            table_offset,
            pba_offset,
            table,
            cap: None,
            enabled: false,
        }
    }

    /// Bytes of BAR space the table needs.
    pub fn table_len(&self) -> u64 {
        self.table.len() as u64 * MSIX_ENTRY_SIZE
    }

    /// Bytes of BAR space the PBA needs (one bit per vector, in qwords).
    pub fn pba_len(&self) -> u64 {
        self.table.len().div_ceil(64) as u64 * 8
    }

    /// Append the capability. Only message control bits 14 and 15 are writable.
    pub fn add_capability(&mut self, config: &mut ConfigSpace) -> Result<u8, PciError> {
        let control = (self.table.len() as u16 - 1).to_le_bytes();
        let table = (self.table_offset | u32::from(self.bar)).to_le_bytes();
        let pba = (self.pba_offset | u32::from(self.bar)).to_le_bytes();
        let mut body = Vec::with_capacity(10);
        body.extend_from_slice(&control);
        body.extend_from_slice(&table);
        body.extend_from_slice(&pba);
        let wmask = (CONTROL_ENABLE | CONTROL_FUNCTION_MASK).to_le_bytes();
        let offset = config.add_capability(MSIX_CAP_ID, &body, &wmask)?;
        self.cap = Some(offset);
        tracing::debug!(
            target: "ternvale::pci",
            function = %self.name,
            offset = format!("{offset:#x}"),
            vectors = self.table.len(),
            "msi-x capability added"
        );
        Ok(offset)
    }

    /// Call after every config write; logs enable and function-mask changes.
    pub fn sync(&mut self, config: &ConfigSpace) {
        let Some(cap) = self.cap else {
            return;
        };
        let control = config.u16_at(usize::from(cap) + 2);
        let enabled = control & CONTROL_ENABLE != 0;
        if enabled != self.enabled {
            self.enabled = enabled;
            tracing::warn!(
                target: "ternvale::pci",
                function = %self.name,
                enabled,
                function_mask = control & CONTROL_FUNCTION_MASK != 0,
                "msi-x enable changed; delivery is a stub and interrupts stay on intx"
            );
        }
    }

    /// The guest has set the MSI-X enable bit.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Is `offset` in BAR `bar` inside the table or PBA?
    pub fn claims(&self, bar: usize, offset: u64) -> bool {
        bar == usize::from(self.bar) && (self.in_table(offset) || self.in_pba(offset))
    }

    /// Guest read of the table or PBA. The PBA always reads 0 (nothing pending).
    pub fn read(&self, offset: u64, size: u8) -> u64 {
        if !self.in_table(offset) {
            return 0;
        }
        let rel = offset - u64::from(self.table_offset);
        let entry = &self.table[(rel / MSIX_ENTRY_SIZE) as usize];
        let word = ((rel % MSIX_ENTRY_SIZE) / 4) as usize;
        let value = u64::from(entry[word]);
        if size == 8 && word % 2 == 0 {
            value | (u64::from(entry[word + 1]) << 32)
        } else {
            value
        }
    }

    /// Guest write of a table entry. PBA writes are ignored.
    pub fn write(&mut self, offset: u64, size: u8, value: u64) {
        if !self.in_table(offset) {
            tracing::warn!(target: "ternvale::pci", function = %self.name, offset = format!("{offset:#x}"), "write to msi-x pba ignored");
            return;
        }
        let rel = offset - u64::from(self.table_offset);
        let vector = (rel / MSIX_ENTRY_SIZE) as usize;
        let word = ((rel % MSIX_ENTRY_SIZE) / 4) as usize;
        let entry = &mut self.table[vector];
        entry[word] = value as u32;
        if size == 8 && word % 2 == 0 {
            entry[word + 1] = (value >> 32) as u32;
        }
        tracing::debug!(
            target: "ternvale::pci",
            function = %self.name,
            vector,
            address = format!("{:#x}", (u64::from(entry[1]) << 32) | u64::from(entry[0])),
            data = format!("{:#x}", entry[2]),
            masked = entry[3] & ENTRY_MASKED != 0,
            "msi-x table entry"
        );
    }

    /// Would deliver `vector`; returns whether it was delivered (always false).
    pub fn signal(&self, vector: u16) -> bool {
        tracing::warn!(
            target: "ternvale::pci",
            function = %self.name,
            vector,
            "msi-x delivery is not implemented; message dropped"
        );
        false
    }

    fn in_table(&self, offset: u64) -> bool {
        let start = u64::from(self.table_offset);
        offset >= start && offset < start + self.table_len()
    }

    fn in_pba(&self, offset: u64) -> bool {
        let start = u64::from(self.pba_offset);
        offset >= start && offset < start + self.pba_len()
    }
}
