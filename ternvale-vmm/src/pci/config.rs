//! Type 0 config space with per-byte write masks.
//!
//! BAR sizing falls out of the masks: a BAR's writable bits are
//! `!(size - 1)` above the read-only type nibble, so writing all-ones and
//! reading back yields `~(size - 1) | flags`. Offsets 0x100..0x1000 (PCIe
//! extended space) read 0, which tells the guest there are no extended
//! capabilities.

use super::PciError;

/// Bytes of modelled config space. ECAM exposes 4096 per function.
pub const CONFIG_SPACE_SIZE: usize = 256;
/// Command register.
pub const COMMAND: usize = 0x04;
/// Status register.
pub const STATUS: usize = 0x06;
/// Capabilities pointer.
pub const CAP_PTR: usize = 0x34;
/// Interrupt line (firmware scratch).
pub const INTERRUPT_LINE: usize = 0x3c;
/// Interrupt pin: 0 none, 1..=4 INTA..INTD.
pub const INTERRUPT_PIN: usize = 0x3d;
/// Command bit 1: decode memory BARs.
pub const COMMAND_MEMORY: u16 = 1 << 1;
/// Command bit 2: the function may master DMA.
pub const COMMAND_MASTER: u16 = 1 << 2;
/// Command bit 10: the function must not assert INTx.
pub const COMMAND_INTX_DISABLE: u16 = 1 << 10;
/// Status bit 3: INTx is asserted (read-only, maintained by the bus).
pub const STATUS_INTERRUPT: u16 = 1 << 3;
/// Status bit 4: the capability list at [`CAP_PTR`] is valid.
pub const STATUS_CAP_LIST: u16 = 1 << 4;

const BAR0: usize = 0x10;
const BARS: usize = 6;
const FIRST_CAP: usize = 0x40;
/// Memory, bus master, parity, SERR#, INTx disable. No I/O space.
const COMMAND_WRITABLE: u16 = 0x0546;

/// Type 0 header register containing byte `offset`, for logs. Accesses that
/// span two registers are named by their first byte.
pub fn register_name(offset: u16) -> &'static str {
    match offset {
        0x00..=0x01 => "vendor_id",
        0x02..=0x03 => "device_id",
        0x04..=0x05 => "command",
        0x06..=0x07 => "status",
        0x08 => "revision",
        0x09..=0x0b => "class_code",
        0x0c => "cache_line_size",
        0x0d => "latency_timer",
        0x0e => "header_type",
        0x0f => "bist",
        0x10..=0x13 => "bar0",
        0x14..=0x17 => "bar1",
        0x18..=0x1b => "bar2",
        0x1c..=0x1f => "bar3",
        0x20..=0x23 => "bar4",
        0x24..=0x27 => "bar5",
        0x28..=0x2b => "cardbus_cis",
        0x2c..=0x2d => "subsystem_vendor_id",
        0x2e..=0x2f => "subsystem_id",
        0x30..=0x33 => "expansion_rom",
        0x34 => "cap_ptr",
        0x35..=0x3b => "reserved",
        0x3c => "interrupt_line",
        0x3d => "interrupt_pin",
        0x3e => "min_gnt",
        0x3f => "max_lat",
        0x40..=0xff => "capability",
        _ => "extended",
    }
}

/// Identity fields of a type 0 header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Vendor ID.
    pub vendor_id: u16,
    /// Device ID.
    pub device_id: u16,
    /// 24-bit class code: base class, subclass, programming interface.
    pub class_code: u32,
    /// Revision ID.
    pub revision: u8,
    /// Subsystem vendor ID.
    pub subsystem_vendor_id: u16,
    /// Subsystem ID.
    pub subsystem_id: u16,
    /// 0 for no INTx, 1..=4 for INTA..INTD.
    pub interrupt_pin: u8,
}

/// Memory BAR width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarKind {
    /// One 32-bit BAR register.
    Mem32,
    /// Two BAR registers; the second holds the upper 32 address bits.
    Mem64,
}

/// One memory BAR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    /// Power-of-two size in bytes, at least 16.
    pub size: u64,
    /// Register width.
    pub kind: BarKind,
    /// Prefetchable flag (bit 3).
    pub prefetchable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarSlot {
    Empty,
    Bar(Bar),
    Upper,
}

/// 256 bytes of config space plus the bits the guest may change.
#[derive(Clone)]
pub struct ConfigSpace {
    data: [u8; CONFIG_SPACE_SIZE],
    wmask: [u8; CONFIG_SPACE_SIZE],
    bars: [BarSlot; BARS],
    next_cap: usize,
    last_cap: Option<usize>,
}

impl std::fmt::Debug for ConfigSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigSpace")
            .field("vendor", &format_args!("{:#06x}", self.u16_at(0)))
            .field("device", &format_args!("{:#06x}", self.u16_at(2)))
            .field("command", &format_args!("{:#06x}", self.command()))
            .finish()
    }
}

impl ConfigSpace {
    /// A single-function type 0 header with no BARs or capabilities.
    pub fn new(header: Header) -> Self {
        let mut space = Self {
            data: [0; CONFIG_SPACE_SIZE],
            wmask: [0; CONFIG_SPACE_SIZE],
            bars: [BarSlot::Empty; BARS],
            next_cap: FIRST_CAP,
            last_cap: None,
        };
        space.put(0x00, &header.vendor_id.to_le_bytes());
        space.put(0x02, &header.device_id.to_le_bytes());
        space.data[0x08] = header.revision;
        space.put(0x09, &header.class_code.to_le_bytes()[..3]);
        space.put(0x2c, &header.subsystem_vendor_id.to_le_bytes());
        space.put(0x2e, &header.subsystem_id.to_le_bytes());
        space.data[INTERRUPT_PIN] = header.interrupt_pin;
        space.wmask[COMMAND..COMMAND + 2].copy_from_slice(&COMMAND_WRITABLE.to_le_bytes());
        // Cache line size, latency timer, interrupt line: plain scratch bytes.
        space.wmask[0x0c] = 0xff;
        space.wmask[0x0d] = 0xff;
        space.wmask[INTERRUPT_LINE] = 0xff;
        space
    }

    /// Declare memory BAR `index`. 64-bit BARs also take `index + 1`.
    pub fn add_bar(&mut self, index: usize, bar: Bar) -> Result<(), PciError> {
        let kind = bar.kind;
        let regs = if kind == BarKind::Mem64 { 2 } else { 1 };
        if index + regs > BARS {
            return Err(PciError::BarIndex { index, kind });
        }
        let max = if kind == BarKind::Mem64 {
            1 << 63
        } else {
            1 << 31
        };
        if bar.size < 16 || bar.size > max || !bar.size.is_power_of_two() {
            return Err(PciError::BarSize {
                size: bar.size,
                kind,
            });
        }
        if let Some(taken) = (index..index + regs).find(|&i| self.bars[i] != BarSlot::Empty) {
            return Err(PciError::BarTaken { index: taken });
        }
        let mut flags = 0u32;
        if kind == BarKind::Mem64 {
            flags |= 0b100;
        }
        if bar.prefetchable {
            flags |= 0b1000;
        }
        let address_mask = !(bar.size - 1);
        let reg = BAR0 + index * 4;
        self.put(reg, &flags.to_le_bytes());
        self.wmask[reg..reg + 4].copy_from_slice(&((address_mask as u32) & !0xf).to_le_bytes());
        self.bars[index] = BarSlot::Bar(bar);
        if kind == BarKind::Mem64 {
            let high = reg + 4;
            self.wmask[high..high + 4]
                .copy_from_slice(&((address_mask >> 32) as u32).to_le_bytes());
            self.bars[index + 1] = BarSlot::Upper;
        }
        Ok(())
    }

    /// Append a capability (`id`, next pointer, then `body`) to the list and
    /// return its offset. `body_wmask` marks guest-writable body bits.
    pub fn add_capability(
        &mut self,
        id: u8,
        body: &[u8],
        body_wmask: &[u8],
    ) -> Result<u8, PciError> {
        let len = 2 + body.len();
        let offset = self.next_cap;
        if offset + len > CONFIG_SPACE_SIZE || body_wmask.len() > body.len() {
            return Err(PciError::CapabilitySpace { id, len });
        }
        self.data[offset] = id;
        self.data[offset + 1] = 0;
        self.put(offset + 2, body);
        self.wmask[offset + 2..offset + 2 + body_wmask.len()].copy_from_slice(body_wmask);
        match self.last_cap {
            Some(prev) => self.data[prev + 1] = offset as u8,
            None => self.data[CAP_PTR] = offset as u8,
        }
        let status = self.u16_at(STATUS) | STATUS_CAP_LIST;
        self.put(STATUS, &status.to_le_bytes());
        self.last_cap = Some(offset);
        self.next_cap = (offset + len + 3) & !3;
        Ok(offset as u8)
    }

    /// Guest read. Out-of-range or extended-space bytes read as 0.
    pub fn read(&self, offset: u16, size: u8) -> u32 {
        let start = usize::from(offset);
        let mut value = 0u32;
        for i in 0..usize::from(size.min(4)) {
            let byte = self.data.get(start + i).copied().unwrap_or(0);
            value |= u32::from(byte) << (8 * i);
        }
        value
    }

    /// Guest write through the write masks. Extended-space writes are dropped.
    pub fn write(&mut self, offset: u16, size: u8, value: u32) {
        let start = usize::from(offset);
        for i in 0..usize::from(size.min(4)) {
            let Some(mask) = self.wmask.get(start + i).copied() else {
                return;
            };
            let byte = (value >> (8 * i)) as u8;
            let old = self.data[start + i];
            self.data[start + i] = (old & !mask) | (byte & mask);
        }
    }

    /// Raw bytes, for functions that keep dynamic fields in a capability.
    pub fn bytes(&self, offset: usize, len: usize) -> &[u8] {
        let end = (offset + len).min(CONFIG_SPACE_SIZE);
        &self.data[offset.min(end)..end]
    }

    /// Overwrite raw bytes, ignoring the write masks. Device-side updates only.
    pub fn set_bytes(&mut self, offset: usize, bytes: &[u8]) {
        self.put(offset, bytes);
    }

    /// Little-endian u16 at `offset`.
    pub fn u16_at(&self, offset: usize) -> u16 {
        self.read(offset as u16, 2) as u16
    }

    /// Little-endian u32 at `offset`.
    pub fn u32_at(&self, offset: usize) -> u32 {
        self.read(offset as u16, 4)
    }

    /// Command register.
    pub fn command(&self) -> u16 {
        self.u16_at(COMMAND)
    }

    /// Memory decode is enabled.
    pub fn memory_enabled(&self) -> bool {
        self.command() & COMMAND_MEMORY != 0
    }

    /// INTx is masked by the driver.
    pub fn intx_disabled(&self) -> bool {
        self.command() & COMMAND_INTX_DISABLE != 0
    }

    /// The BAR declared at `index`, if any.
    pub fn bar(&self, index: usize) -> Option<Bar> {
        match self.bars.get(index) {
            Some(BarSlot::Bar(bar)) => Some(*bar),
            _ => None,
        }
    }

    /// Address currently programmed into BAR `index`.
    pub fn bar_address(&self, index: usize) -> Option<u64> {
        let bar = self.bar(index)?;
        let reg = BAR0 + index * 4;
        let low = u64::from(self.u32_at(reg) & !0xf);
        let high = match bar.kind {
            BarKind::Mem32 => 0,
            BarKind::Mem64 => u64::from(self.u32_at(reg + 4)),
        };
        Some((high << 32) | low)
    }

    /// Program BAR `index` as firmware would. `address` is masked to the BAR size.
    pub fn set_bar_address(&mut self, index: usize, address: u64) {
        let Some(bar) = self.bar(index) else {
            return;
        };
        let address = address & !(bar.size - 1);
        let reg = BAR0 + index * 4;
        let flags = self.u32_at(reg) & 0xf;
        self.put(reg, &((address as u32) | flags).to_le_bytes());
        if bar.kind == BarKind::Mem64 {
            self.put(reg + 4, &((address >> 32) as u32).to_le_bytes());
        }
    }

    /// `(index, base, size)` for every BAR the function decodes right now:
    /// memory decode on and a non-zero address.
    pub fn decoded_bars(&self) -> Vec<(usize, u64, u64)> {
        if !self.memory_enabled() {
            return Vec::new();
        }
        (0..BARS)
            .filter_map(|index| {
                let bar = self.bar(index)?;
                let base = self.bar_address(index)?;
                (base != 0).then_some((index, base, bar.size))
            })
            .collect()
    }

    fn put(&mut self, offset: usize, bytes: &[u8]) {
        let end = (offset + bytes.len()).min(CONFIG_SPACE_SIZE);
        if offset < end {
            self.data[offset..end].copy_from_slice(&bytes[..end - offset]);
        }
    }
}
