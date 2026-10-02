//! Intel/Sharp (CFI command set 0001) NOR flash state machine.
//!
//! Modeled on QEMU's `pflash_cfi01` as `virt` configures it: a 4-byte bank of
//! two interleaved x16 chips with 256 KiB erase blocks. EDK2's
//! `VirtNorFlash.h` therefore sends every command doubled into both 16-bit
//! halves (`CREATE_DUAL_CMD`) and expects every status bit doubled
//! (`P30_SR_BIT_WRITE = 0x00800080`). Operations finish instantly, so the
//! ready bit is always set. Block locking is accepted but not modeled: every
//! block reads back unlocked.

/// Erase block. EDK2 `QEMU_NOR_BLOCK_SIZE`; QEMU `virt` `sector_len`.
pub const BLOCK_SIZE: u64 = 0x4_0000;
/// Largest buffered program. EDK2 `P30_MAX_BUFFER_SIZE_IN_BYTES`.
pub(super) const MAX_BUFFER: usize = 128;

const SR_READY: u8 = 0x80;
const SR_ERASE_ERROR: u8 = 0x20;
const SR_PROGRAM_ERROR: u8 = 0x10;

const CMD_READ_ARRAY: u8 = 0xff;
const CMD_READ_ARRAY_ALT: u8 = 0xf0;
const CMD_READ_STATUS: u8 = 0x70;
const CMD_CLEAR_STATUS: u8 = 0x50;
const CMD_READ_ID: u8 = 0x90;
const CMD_CFI_QUERY: u8 = 0x98;
const CMD_WORD_PROGRAM: u8 = 0x40;
const CMD_WORD_PROGRAM_ALT: u8 = 0x10;
const CMD_BLOCK_ERASE: u8 = 0x20;
const CMD_LOCK_SETUP: u8 = 0x60;
const CMD_BUFFERED_PROGRAM: u8 = 0xe8;
const CMD_CONFIRM: u8 = 0xd0;
const LOCK_BLOCK: u8 = 0x01;
const LOCK_DOWN: u8 = 0x2f;

/// QEMU `virt` pflash ID: Intel (0x89), device 0x18.
const MANUFACTURER_ID: u16 = 0x89;
const DEVICE_ID: u16 = 0x18;

/// What reads return outside a multi-cycle command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// Flash contents.
    Array,
    /// The status register.
    Status,
    /// Manufacturer, device, and block lock status.
    Ident,
    /// CFI query table.
    Query,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    None,
    Program,
    Erase,
    Lock,
    BufferCount {
        start: u64,
    },
    BufferData {
        start: u64,
        data: Vec<u8>,
        got: usize,
    },
    BufferConfirm {
        start: u64,
        data: Vec<u8>,
    },
}

/// Bytes the guest changed; the caller persists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Changed {
    pub offset: u64,
    pub len: usize,
}

/// One flash bank's command state.
#[derive(Debug)]
pub(super) struct Cfi {
    mode: Mode,
    pending: Pending,
    status: u8,
}

impl Cfi {
    pub(super) fn new() -> Self {
        Self {
            mode: Mode::Array,
            pending: Pending::None,
            status: SR_READY,
        }
    }

    /// True when reads may go straight to the backing pages.
    pub(super) fn array_mode(&self) -> bool {
        self.mode == Mode::Array && self.pending == Pending::None
    }

    pub(super) fn mode(&self) -> Mode {
        self.mode
    }

    pub(super) fn status(&self) -> u8 {
        self.status
    }

    /// Flag a failed program/erase (for example the NVRAM write failed).
    pub(super) fn fail_program(&mut self) {
        self.status |= SR_PROGRAM_ERROR;
    }

    /// Read `size` bytes at `offset`. `storage` is the whole bank.
    pub(super) fn read(&self, storage: &[u8], offset: u64, size: u8) -> u64 {
        let lane = (offset & 3) * 8;
        let value = match self.mode {
            Mode::Array if self.pending == Pending::None => return read_le(storage, offset, size),
            Mode::Ident => dual(ident((offset % BLOCK_SIZE) >> 2)) >> lane,
            Mode::Query => dual(cfi_query(offset >> 2)) >> lane,
            _ => dual(u16::from(self.status)) >> lane,
        };
        value & mask(size)
    }

    /// Apply a guest write. `Err` carries a WARN reason; the status register
    /// already shows the failure.
    pub(super) fn write(
        &mut self,
        storage: &mut [u8],
        offset: u64,
        size: u8,
        value: u64,
    ) -> Result<Option<Changed>, &'static str> {
        let end = offset.saturating_add(u64::from(size));
        if end > storage.len() as u64 {
            return self.reject("write past the end of the bank");
        }
        let cmd = value as u8;
        match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::None => self.command(offset, size, value),
            Pending::Program => {
                write_le(storage, offset, size, value);
                self.mode = Mode::Status;
                Ok(Some(Changed {
                    offset,
                    len: usize::from(size),
                }))
            }
            Pending::Erase if cmd == CMD_CONFIRM => {
                let start = offset - offset % BLOCK_SIZE;
                let len = (BLOCK_SIZE as usize).min(storage.len() - start as usize);
                storage[start as usize..start as usize + len].fill(0xff);
                self.mode = Mode::Status;
                Ok(Some(Changed { offset: start, len }))
            }
            Pending::Lock if matches!(cmd, LOCK_BLOCK | CMD_CONFIRM | LOCK_DOWN) => {
                self.mode = Mode::Status;
                Ok(None)
            }
            Pending::BufferCount { start } => {
                let bytes = ((value & 0xffff) as usize + 1) * 4;
                if bytes > MAX_BUFFER || start + bytes as u64 > storage.len() as u64 {
                    return self.reject("buffered program count out of range");
                }
                self.pending = Pending::BufferData {
                    start,
                    data: vec![0xff; bytes],
                    got: 0,
                };
                Ok(None)
            }
            Pending::BufferData {
                start,
                mut data,
                got,
            } => {
                let Some(at) = offset
                    .checked_sub(start)
                    .filter(|at| at + u64::from(size) <= data.len() as u64)
                else {
                    return self.reject("buffered program data outside the buffer");
                };
                let at = at as usize;
                data[at..at + usize::from(size)]
                    .copy_from_slice(&value.to_le_bytes()[..usize::from(size)]);
                let got = got + usize::from(size);
                self.pending = if got >= data.len() {
                    Pending::BufferConfirm { start, data }
                } else {
                    Pending::BufferData { start, data, got }
                };
                Ok(None)
            }
            Pending::BufferConfirm { start, data } if cmd == CMD_CONFIRM => {
                let at = start as usize;
                storage[at..at + data.len()].copy_from_slice(&data);
                self.mode = Mode::Status;
                Ok(Some(Changed {
                    offset: start,
                    len: data.len(),
                }))
            }
            Pending::Erase | Pending::Lock | Pending::BufferConfirm { .. } => {
                self.reject("unexpected confirm cycle")
            }
        }
    }

    fn command(
        &mut self,
        offset: u64,
        size: u8,
        value: u64,
    ) -> Result<Option<Changed>, &'static str> {
        let cmd = value as u8;
        if size >= 4 && (value >> 16) as u8 != cmd {
            tracing::debug!(
                target: "ternvale::flash",
                value = format!("{value:#x}"),
                "command halves differ; using the low chip"
            );
        }
        match cmd {
            CMD_READ_ARRAY | CMD_READ_ARRAY_ALT => self.mode = Mode::Array,
            CMD_READ_STATUS => self.mode = Mode::Status,
            CMD_CLEAR_STATUS => {
                self.status = SR_READY;
                self.mode = Mode::Array;
            }
            CMD_READ_ID => self.mode = Mode::Ident,
            CMD_CFI_QUERY => self.mode = Mode::Query,
            CMD_WORD_PROGRAM | CMD_WORD_PROGRAM_ALT => self.start(Pending::Program),
            CMD_BLOCK_ERASE => self.start(Pending::Erase),
            CMD_LOCK_SETUP => self.start(Pending::Lock),
            CMD_BUFFERED_PROGRAM => self.start(Pending::BufferCount { start: offset }),
            _ => return self.reject("unknown flash command"),
        }
        Ok(None)
    }

    fn start(&mut self, pending: Pending) {
        self.pending = pending;
        self.mode = Mode::Status;
    }

    fn reject(&mut self, reason: &'static str) -> Result<Option<Changed>, &'static str> {
        self.pending = Pending::None;
        self.status |= SR_ERASE_ERROR | SR_PROGRAM_ERROR;
        self.mode = Mode::Status;
        Err(reason)
    }
}

/// Replicate a per-chip 16-bit value into both halves of each 32-bit word.
fn dual(chip: u16) -> u64 {
    let word = u64::from(chip) | (u64::from(chip) << 16);
    word | (word << 32)
}

fn ident(index: u64) -> u16 {
    match index {
        0 => MANUFACTURER_ID,
        1 => DEVICE_ID,
        _ => 0,
    }
}

/// Minimal CFI table for one x16 chip (half the bank). EDK2 never queries it;
/// it is here for drivers that probe (Linux `physmap-flash`).
/// TODO(verify): no guest has exercised this table; compare with QEMU
/// `pflash_cfi01` (`cfi_table`) before relying on it.
fn cfi_query(index: u64) -> u16 {
    const CHIP_BLOCK: u64 = BLOCK_SIZE / 2;
    match index {
        0x10 => u16::from(b'Q'),
        0x11 => u16::from(b'R'),
        0x12 => u16::from(b'Y'),
        0x13 => 0x01, // primary command set 0001
        0x27 => 25,   // 32 MiB per chip
        0x28 => 0x01, // x16 interface
        0x2a => 6,    // 64-byte write buffer per chip
        0x2c => 1,    // one erase block region
        0x2d => 0xff, // 256 blocks - 1, low byte
        0x2f => ((CHIP_BLOCK >> 8) & 0xff) as u16,
        0x30 => ((CHIP_BLOCK >> 16) & 0xff) as u16,
        _ => 0,
    }
}

fn mask(size: u8) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (u32::from(size) * 8)) - 1
    }
}

fn read_le(storage: &[u8], offset: u64, size: u8) -> u64 {
    let mut bytes = [0u8; 8];
    let at = offset as usize;
    let len = usize::from(size.min(8));
    if let Some(src) = storage.get(at..at + len) {
        bytes[..len].copy_from_slice(src);
    }
    u64::from_le_bytes(bytes)
}

fn write_le(storage: &mut [u8], offset: u64, size: u8, value: u64) {
    let at = offset as usize;
    let len = usize::from(size.min(8));
    if let Some(dst) = storage.get_mut(at..at + len) {
        dst.copy_from_slice(&value.to_le_bytes()[..len]);
    }
}
