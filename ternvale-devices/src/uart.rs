//! PL011 UART at guest physical [`PL011_BASE`].
//!
//! Register offsets follow ARM DDI 0183. PeriphID and PCellID match the ARM
//! PrimeCell values used by QEMU's PL011 (`11 10 14 00 0d f0 05 b1`).
//!
//! A write to UARTDR transmits even when `UARTEN` is clear. The bare-metal
//! hello payload stores a byte and never programs the control register.
//! TODO(verify): the PL011 TRM requires `UARTEN` before transmission.

mod output;

use std::collections::VecDeque;

pub use output::{ByteSink, StdoutFile, UartError};

use output::LineBuf;

/// Guest physical base. The device itself is addressed by offset.
pub const PL011_BASE: u64 = 0x0900_0000;
/// MMIO window covering the register block through PCellID3.
pub const PL011_SIZE: u64 = 0x1000;

const DR: u64 = 0x000;
const FR: u64 = 0x018;
const IBRD: u64 = 0x024;
const FBRD: u64 = 0x028;
const LCR_H: u64 = 0x02c;
const CR: u64 = 0x030;
const IMSC: u64 = 0x038;
const RIS: u64 = 0x03c;
const MIS: u64 = 0x040;
const ICR: u64 = 0x044;
const PERIPH_ID0: u64 = 0xfe0;

const FR_TXFE: u32 = 1 << 7;
const FR_RXFF: u32 = 1 << 6;
const FR_TXFF: u32 = 1 << 5;
const FR_RXFE: u32 = 1 << 4;
const LCR_FEN: u16 = 1 << 4;
const IRQ_RX: u16 = 1 << 4;
const IRQ_TX: u16 = 1 << 5;
/// `UARTCR` reset: TX and RX enabled, UART disabled.
const CR_RESET: u16 = 0x0300;

const PERIPH_ID: [u8; 4] = [0x11, 0x10, 0x14, 0x00];
const PCELL_ID: [u8; 4] = [0x0d, 0xf0, 0x05, 0xb1];

/// PrimeCell PL011.
pub struct Pl011 {
    sink: Box<dyn ByteSink>,
    line: LineBuf,
    rx: VecDeque<u8>,
    ibrd: u16,
    fbrd: u8,
    lcr_h: u16,
    cr: u16,
    imsc: u16,
    ris: u16,
}

impl Pl011 {
    /// UART that writes guest TX into `sink`.
    #[tracing::instrument(level = "debug", target = "ternvale::uart", skip_all)]
    pub fn new(sink: Box<dyn ByteSink>) -> Self {
        tracing::info!(target: "ternvale::uart", base = format!("{:#x}", PL011_BASE), "pl011 created");
        Self {
            sink,
            line: LineBuf::default(),
            rx: VecDeque::new(),
            ibrd: 0,
            fbrd: 0,
            lcr_h: 0,
            cr: CR_RESET,
            imsc: 0,
            ris: 0,
        }
    }

    /// UART that writes stdout and appends `serial_log`.
    #[tracing::instrument(level = "debug", target = "ternvale::uart", skip_all, fields(path = %serial_log.display()))]
    pub fn open(serial_log: &std::path::Path) -> Result<Self, UartError> {
        Ok(Self::new(Box::new(StdoutFile::create(serial_log)?)))
    }

    /// Queue one byte from the host. `false` when the RX FIFO is full.
    #[tracing::instrument(level = "debug", target = "ternvale::uart", skip_all)]
    pub fn push_rx(&mut self, byte: u8) -> bool {
        if self.rx.len() >= self.fifo_depth() {
            tracing::warn!(target: "ternvale::uart", "rx fifo full; dropped host byte");
            return false;
        }
        self.rx.push_back(byte);
        self.ris |= IRQ_RX;
        self.note_irq();
        true
    }

    /// Read a register. `offset` is from [`PL011_BASE`].
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::uart",
        skip_all,
        fields(offset = format!("{:#x}", offset), size)
    )]
    pub fn read(&mut self, offset: u64, size: u8) -> u64 {
        let value = self.read_reg(offset);
        self.trace(offset, size, value, "read");
        mask(value, size)
    }

    /// Write a register. `offset` is from [`PL011_BASE`].
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::uart",
        skip_all,
        fields(offset = format!("{:#x}", offset), size, value = format!("{:#x}", value))
    )]
    pub fn write(&mut self, offset: u64, size: u8, value: u64) {
        let value = mask(value, size);
        self.trace(offset, size, value, "write");
        self.write_reg(offset, value);
    }

    fn read_reg(&mut self, offset: u64) -> u64 {
        match offset {
            DR => self.read_dr(),
            FR => u64::from(self.flags()),
            IBRD => u64::from(self.ibrd),
            FBRD => u64::from(self.fbrd),
            LCR_H => u64::from(self.lcr_h),
            CR => u64::from(self.cr),
            IMSC => u64::from(self.imsc),
            RIS => u64::from(self.ris),
            MIS => u64::from(self.ris & self.imsc),
            ICR => 0,
            id if (PERIPH_ID0..PERIPH_ID0 + 32).contains(&id) && id % 4 == 0 => {
                u64::from(id_byte(id))
            }
            _ => {
                tracing::warn!(
                    target: "ternvale::uart",
                    offset = format!("{:#x}", offset),
                    "read of unknown pl011 register"
                );
                0
            }
        }
    }

    fn write_reg(&mut self, offset: u64, value: u64) {
        match offset {
            DR => self.transmit(value as u8),
            IBRD => self.ibrd = value as u16,
            FBRD => self.fbrd = (value as u8) & 0x3f,
            LCR_H => self.lcr_h = (value as u16) & 0xff,
            CR => self.cr = value as u16,
            IMSC => {
                self.imsc = (value as u16) & 0x7ff;
                self.note_irq();
            }
            ICR => {
                self.ris &= !(value as u16);
                self.note_irq();
            }
            FR | RIS | MIS | PERIPH_ID0..=0xffc => {
                tracing::warn!(
                    target: "ternvale::uart",
                    offset = format!("{:#x}", offset),
                    "ignored write to read-only pl011 register"
                );
            }
            _ => tracing::warn!(
                target: "ternvale::uart",
                offset = format!("{:#x}", offset),
                "ignored write to unknown pl011 register"
            ),
        }
    }

    fn read_dr(&mut self) -> u64 {
        match self.rx.pop_front() {
            Some(byte) => {
                if self.rx.is_empty() {
                    self.ris &= !IRQ_RX;
                }
                u64::from(byte)
            }
            None => {
                tracing::warn!(target: "ternvale::uart", "uart dr read while rx empty");
                0
            }
        }
    }

    fn transmit(&mut self, byte: u8) {
        if let Err(error) = self.sink.write_all(&[byte]) {
            tracing::error!(target: "ternvale::uart", error = %error, "uart tx sink write failed");
        }
        if let Some(line) = self.line.push(byte) {
            tracing::debug!(target: "ternvale::uart", line = %line, "uart tx");
        }
        self.ris |= IRQ_TX;
        self.note_irq();
    }

    fn flags(&self) -> u32 {
        let mut flags = FR_TXFE;
        if self.rx.is_empty() {
            flags |= FR_RXFE;
        }
        if self.rx.len() >= self.fifo_depth() {
            flags |= FR_RXFF;
        }
        debug_assert_eq!(flags & FR_TXFF, 0, "tx is drained immediately");
        flags
    }

    fn fifo_depth(&self) -> usize {
        if self.lcr_h & LCR_FEN != 0 {
            16
        } else {
            1
        }
    }

    fn note_irq(&self) {
        let masked = self.ris & self.imsc;
        if masked != 0 {
            tracing::debug!(
                target: "ternvale::uart",
                mis = format!("{:#x}", masked),
                "uart interrupt stub"
            );
        }
    }

    fn trace(&self, offset: u64, size: u8, value: u64, direction: &'static str) {
        tracing::trace!(
            target: "ternvale::uart",
            reg = reg_name(offset),
            offset = format!("{:#x}", offset),
            size,
            value = format!("{:#x}", value),
            direction,
            "uart register"
        );
    }
}

impl Drop for Pl011 {
    fn drop(&mut self) {
        if let Some(line) = self.line.flush() {
            tracing::debug!(target: "ternvale::uart", line = %line, "uart tx");
        }
    }
}

impl ternvale_vmm::MmioDevice for Pl011 {
    fn name(&self) -> &str {
        "pl011"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        Pl011::read(self, offset, size)
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        Pl011::write(self, offset, size, val);
    }
}

fn id_byte(offset: u64) -> u8 {
    let index = ((offset - PERIPH_ID0) / 4) as usize;
    if index < 4 {
        PERIPH_ID[index]
    } else {
        PCELL_ID[index - 4]
    }
}

fn reg_name(offset: u64) -> &'static str {
    match offset {
        DR => "UARTDR",
        FR => "UARTFR",
        IBRD => "UARTIBRD",
        FBRD => "UARTFBRD",
        LCR_H => "UARTLCR_H",
        CR => "UARTCR",
        IMSC => "UARTIMSC",
        RIS => "UARTRIS",
        MIS => "UARTMIS",
        ICR => "UARTICR",
        0xfe0 => "UARTPeriphID0",
        0xfe4 => "UARTPeriphID1",
        0xfe8 => "UARTPeriphID2",
        0xfec => "UARTPeriphID3",
        0xff0 => "UARTPCellID0",
        0xff4 => "UARTPCellID1",
        0xff8 => "UARTPCellID2",
        0xffc => "UARTPCellID3",
        _ => "unknown",
    }
}

fn mask(value: u64, size: u8) -> u64 {
    match size {
        1 => value & 0xff,
        2 => value & 0xffff,
        4 => value & 0xffff_ffff,
        _ => value,
    }
}

#[cfg(test)]
#[path = "uart_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "uart_hv_test.rs"]
mod hv_test;
