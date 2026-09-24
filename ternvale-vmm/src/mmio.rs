//! Guest MMIO dispatch. Devices own GPA ranges that must not overlap.
//!
//! A read result is written back to the destination register, zero-extended to
//! the access size. Drop logs each device's access count.

use crate::esr::ExitEvent;
use crate::vcpu::{Vcpu, VcpuError};

/// A device behind one guest-physical window.
pub trait MmioDevice {
    /// Short name used in logs and the shutdown counter dump.
    fn name(&self) -> &str;

    /// Read `size` bytes at `offset` from the start of this device's window.
    fn read(&mut self, offset: u64, size: u8) -> u64;

    /// Write `val` at `offset`. `val` is already masked to `size` bytes.
    fn write(&mut self, offset: u64, size: u8, val: u64);
}

/// Guest general-purpose registers touched by an MMIO exit.
pub trait GuestRegs {
    /// Read `Xn`.
    fn get_reg(&self, index: u8) -> Result<u64, MmioError>;

    /// Write `Xn`.
    fn set_reg(&self, index: u8, value: u64) -> Result<(), MmioError>;
}

impl GuestRegs for Vcpu {
    fn get_reg(&self, index: u8) -> Result<u64, MmioError> {
        self.get_x(index).map_err(MmioError::from)
    }

    fn set_reg(&self, index: u8, value: u64) -> Result<(), MmioError> {
        self.set_x(index, value).map_err(MmioError::from)
    }
}

/// Registering a device or completing an access failed.
#[derive(Debug, thiserror::Error)]
pub enum MmioError {
    /// The new window shares bytes with one already registered.
    #[error("mmio range {base:#x} size {size:#x} overlaps {other_base:#x} size {other_size:#x}")]
    Overlap {
        /// Start of the rejected window.
        base: u64,
        /// Length of the rejected window.
        size: u64,
        /// Start of the window already registered.
        other_base: u64,
        /// Length of the window already registered.
        other_size: u64,
    },

    /// A window length was zero.
    #[error("mmio range {base:#x} has size 0")]
    Empty {
        /// Start of the rejected window.
        base: u64,
    },

    /// `base + size` does not fit in a `u64`.
    #[error("mmio range {base:#x} size {size:#x} overflows")]
    Overflow {
        /// Start of the rejected window or access.
        base: u64,
        /// Length that overflowed.
        size: u64,
    },

    /// The exit access size was not 1, 2, 4, or 8.
    #[error("mmio access size {size} is not 1, 2, 4, or 8")]
    BadSize {
        /// Guest-supplied size in bytes.
        size: u8,
    },

    /// `SRT` was not a general-purpose register.
    #[error("mmio register {reg} is outside 0..=30")]
    BadReg {
        /// Guest-supplied register index.
        reg: u8,
    },

    /// The exit was not [`ExitEvent::Mmio`].
    #[error("exit is not an mmio data abort")]
    NotMmio,

    /// Reading or writing the destination register failed.
    #[error("mmio register access: {0}")]
    Vcpu(#[from] VcpuError),
}

struct Slot {
    base: u64,
    size: u64,
    device: Box<dyn MmioDevice>,
    accesses: u64,
}

/// Registered MMIO devices, dispatched from [`ExitEvent::Mmio`].
pub struct MmioBus {
    devices: Vec<Slot>,
}

impl std::fmt::Debug for MmioBus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MmioBus")
            .field("devices", &self.devices.len())
            .finish()
    }
}

impl Default for MmioBus {
    fn default() -> Self {
        Self::new()
    }
}

impl MmioBus {
    /// An empty bus.
    #[tracing::instrument(level = "debug", target = "ternvale::mmio", skip_all)]
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
        }
    }

    /// Register `device` at `[base, base + size)`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::mmio",
        skip_all,
        fields(device = device.name(), base = format!("{:#x}", base), size = format!("{:#x}", size))
    )]
    pub fn register(
        &mut self,
        base: u64,
        size: u64,
        device: Box<dyn MmioDevice>,
    ) -> Result<(), MmioError> {
        if size == 0 {
            tracing::warn!(target: "ternvale::mmio", base = format!("{:#x}", base), "rejected empty mmio range");
            return Err(MmioError::Empty { base });
        }
        let Some(end) = base.checked_add(size) else {
            tracing::warn!(
                target: "ternvale::mmio",
                base = format!("{:#x}", base),
                size = format!("{:#x}", size),
                "rejected overflowing mmio range"
            );
            return Err(MmioError::Overflow { base, size });
        };
        if let Some(other) = self.devices.iter().find(|slot| {
            let other_end = slot.base + slot.size;
            base < other_end && slot.base < end
        }) {
            let error = MmioError::Overlap {
                base,
                size,
                other_base: other.base,
                other_size: other.size,
            };
            tracing::warn!(target: "ternvale::mmio", error = %error, "rejected overlapping mmio range");
            return Err(error);
        }
        tracing::debug!(
            target: "ternvale::mmio",
            device = device.name(),
            base = format!("{:#x}", base),
            size = format!("{:#x}", size),
            "registered mmio device"
        );
        self.devices.push(Slot {
            base,
            size,
            device,
            accesses: 0,
        });
        Ok(())
    }

    /// How many accesses `name` has handled. `None` if it is not registered.
    #[tracing::instrument(level = "debug", target = "ternvale::mmio", skip_all, fields(device = name))]
    pub fn access_count(&self, name: &str) -> Option<u64> {
        self.devices
            .iter()
            .find(|slot| slot.device.name() == name)
            .map(|slot| slot.accesses)
    }

    /// Perform one [`ExitEvent::Mmio`] against the matching device.
    ///
    /// Reads store the zero-extended result in `event.reg`. Writes take the
    /// low `size` bytes of that register. An address outside every window
    /// returns 0 on read and is ignored on write.
    #[tracing::instrument(level = "debug", target = "ternvale::mmio", skip_all)]
    pub fn dispatch(&mut self, regs: &impl GuestRegs, event: ExitEvent) -> Result<(), MmioError> {
        let ExitEvent::Mmio {
            gpa,
            size,
            write,
            reg,
        } = event
        else {
            tracing::warn!(target: "ternvale::mmio", ?event, "ignored a non-mmio exit");
            return Err(MmioError::NotMmio);
        };
        if !matches!(size, 1 | 2 | 4 | 8) {
            tracing::warn!(target: "ternvale::mmio", size, "rejected mmio size");
            return Err(MmioError::BadSize { size });
        }
        if reg > 30 {
            tracing::warn!(target: "ternvale::mmio", reg, "rejected mmio register");
            return Err(MmioError::BadReg { reg });
        }
        let Some(end) = gpa.checked_add(u64::from(size)) else {
            tracing::warn!(
                target: "ternvale::mmio",
                gpa = format!("{:#x}", gpa),
                size,
                "rejected overflowing mmio access"
            );
            return Err(MmioError::Overflow {
                base: gpa,
                size: u64::from(size),
            });
        };
        let Some(index) = self.devices.iter().position(|slot| {
            let slot_end = slot.base + slot.size;
            gpa >= slot.base && end <= slot_end
        }) else {
            return self.unmapped(regs, gpa, size, write, reg);
        };
        let slot = &mut self.devices[index];
        let offset = gpa - slot.base;
        let direction = if write { "write" } else { "read" };
        let value = if write {
            let raw = regs.get_reg(reg)?;
            let value = mask(raw, size);
            slot.device.write(offset, size, value);
            value
        } else {
            let value = mask(slot.device.read(offset, size), size);
            regs.set_reg(reg, value)?;
            value
        };
        slot.accesses += 1;
        tracing::trace!(
            target: "ternvale::mmio",
            device = slot.device.name(),
            offset = format!("{:#x}", offset),
            size,
            value = format!("{:#x}", value),
            direction,
            "mmio access"
        );
        Ok(())
    }

    fn unmapped(
        &self,
        regs: &impl GuestRegs,
        gpa: u64,
        size: u8,
        write: bool,
        reg: u8,
    ) -> Result<(), MmioError> {
        let direction = if write { "write" } else { "read" };
        tracing::warn!(
            target: "ternvale::mmio",
            gpa = format!("{:#x}", gpa),
            size,
            direction,
            "unmapped mmio access"
        );
        let value = if write {
            mask(regs.get_reg(reg)?, size)
        } else {
            regs.set_reg(reg, 0)?;
            0
        };
        tracing::trace!(
            target: "ternvale::mmio",
            device = "unmapped",
            offset = format!("{:#x}", gpa),
            size,
            value = format!("{:#x}", value),
            direction,
            "mmio access"
        );
        Ok(())
    }
}

impl Drop for MmioBus {
    fn drop(&mut self) {
        for slot in &self.devices {
            tracing::debug!(
                target: "ternvale::mmio",
                device = slot.device.name(),
                accesses = slot.accesses,
                base = format!("{:#x}", slot.base),
                "mmio access count"
            );
        }
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
#[path = "mmio_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mmio_hv_test.rs"]
mod hv_test;
