//! Guest MMIO dispatch. Devices own GPA ranges that must not overlap.
//!
//! A read result is written back to the destination register, zero-extended to
//! the access size. Drop logs each device's access count.
//!
//! Registration needs `&mut self` and happens before any vCPU runs. After that
//! the window list is read-only and [`MmioBus::dispatch`] takes `&self`: each
//! device sits behind its own mutex, so vCPUs only contend on the same device.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(feature = "metrics")]
use std::time::Instant;

use crate::diag::MmioTrace;
use crate::esr::ExitEvent;
use crate::lockwatch::Guard;
use crate::vcpu::{Vcpu, VcpuError};
use ternvale_metrics::Metrics;

/// A device behind one guest-physical window.
///
/// Every vCPU thread dispatches into the same bus, so devices must be `Send`.
/// The bus serializes access to each device with its own mutex.
pub trait MmioDevice: Send {
    /// Short name used in logs and the shutdown counter dump.
    fn name(&self) -> &str;

    /// Read `size` bytes at `offset` from the start of this device's window.
    fn read(&mut self, offset: u64, size: u8) -> u64;

    /// Write `val` at `offset`. `val` is already masked to `size` bytes.
    fn write(&mut self, offset: u64, size: u8, val: u64);

    /// Optional lock-free kick (virtio `QueueNotify`). `None` uses the mutex path.
    fn kick_port(&self) -> Option<Arc<dyn KickPort>> {
        None
    }
}

/// A write that can complete without taking the device mutex.
pub trait KickPort: Send + Sync {
    /// `true` when this write was a notify and the device was kicked.
    fn try_kick(&self, offset: u64, val: u64) -> bool;
}

/// Guest general-purpose registers touched by an MMIO exit.
pub trait GuestRegs {
    /// Read `Xn`.
    fn get_reg(&self, index: u8) -> Result<u64, MmioError>;

    /// Write `Xn`.
    fn set_reg(&self, index: u8, value: u64) -> Result<(), MmioError>;

    /// Kernel vCPU id, recorded with each access in the MMIO trace.
    fn cpu_id(&self) -> Option<u64> {
        None
    }
}

impl GuestRegs for Vcpu {
    fn get_reg(&self, index: u8) -> Result<u64, MmioError> {
        self.get_x(index).map_err(MmioError::from)
    }

    fn set_reg(&self, index: u8, value: u64) -> Result<(), MmioError> {
        self.set_x(index, value).map_err(MmioError::from)
    }

    fn cpu_id(&self) -> Option<u64> {
        Some(self.id())
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
    #[error("mmio register {reg} is outside 0..=31")]
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
    name: Arc<str>,
    device: Mutex<Box<dyn MmioDevice>>,
    kick: Option<Arc<dyn KickPort>>,
    accesses: Arc<AtomicU64>,
    trace_id: Option<u16>,
    #[cfg_attr(not(feature = "metrics"), allow(dead_code))]
    metrics_id: Option<u8>,
}

impl Slot {
    fn lock(&self) -> Guard<'_, Box<dyn MmioDevice>> {
        crate::lockwatch::lock_named(&self.device, Arc::clone(&self.name))
    }

    fn contains(&self, gpa: u64, end: u64) -> bool {
        gpa >= self.base && end <= self.base + self.size
    }
}

/// Registered MMIO devices, dispatched from [`ExitEvent::Mmio`].
pub struct MmioBus {
    devices: Vec<Slot>,
    generation: AtomicU64,
    trace: Option<Arc<MmioTrace>>,
    #[cfg_attr(not(feature = "metrics"), allow(dead_code))]
    metrics: Option<Arc<Metrics>>,
}

thread_local! {
    /// One last-hit window per vCPU thread (`(bus ptr, generation, index)`).
    static LAST_HIT: Cell<(usize, u64, usize)> = const { Cell::new((0, 0, usize::MAX)) };
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
            generation: AtomicU64::new(1),
            trace: None,
            metrics: None,
        }
    }

    /// An empty bus that records every access in `trace`.
    #[tracing::instrument(level = "debug", target = "ternvale::mmio", skip_all)]
    pub fn with_trace(trace: Arc<MmioTrace>) -> Self {
        Self {
            devices: Vec::new(),
            generation: AtomicU64::new(1),
            trace: Some(trace),
            metrics: None,
        }
    }

    /// Attach lock-free MMIO latency counters.
    #[tracing::instrument(level = "debug", target = "ternvale::mmio", skip_all)]
    pub fn set_metrics(&mut self, metrics: Arc<Metrics>) {
        self.metrics = Some(metrics);
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
            let other_end = slot.base.saturating_add(slot.size);
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
        let accesses = Arc::new(AtomicU64::new(0));
        let metrics_id = self
            .metrics
            .as_ref()
            .and_then(|metrics| metrics.register_device(device.name()));
        let trace_id = self
            .trace
            .as_ref()
            .map(|trace| trace.add_window(device.name(), base, size, Arc::clone(&accesses)));
        let kick = device.kick_port();
        let slot = Slot {
            base,
            size,
            name: Arc::from(device.name()),
            device: Mutex::new(device),
            kick,
            accesses,
            trace_id,
            metrics_id,
        };
        let index = self.devices.partition_point(|other| other.base < base);
        self.devices.insert(index, slot);
        self.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    fn lookup(&self, gpa: u64, end: u64) -> Option<usize> {
        let bus = self as *const Self as usize;
        let gen = self.generation.load(Ordering::Acquire);
        LAST_HIT.with(|cell| {
            let (cached_bus, cached_gen, cached_idx) = cell.get();
            if cached_bus == bus && cached_gen == gen {
                if let Some(slot) = self.devices.get(cached_idx) {
                    if slot.contains(gpa, end) {
                        return Some(cached_idx);
                    }
                }
            }
            let found = self.lookup_sorted(gpa, end);
            if let Some(index) = found {
                cell.set((bus, gen, index));
            }
            found
        })
    }

    fn lookup_sorted(&self, gpa: u64, end: u64) -> Option<usize> {
        let index = self.devices.partition_point(|slot| slot.base <= gpa);
        if index == 0 {
            return None;
        }
        let index = index - 1;
        let slot = &self.devices[index];
        if slot.contains(gpa, end) {
            Some(index)
        } else {
            None
        }
    }

    /// How many accesses `name` has handled. `None` if it is not registered.
    #[tracing::instrument(level = "debug", target = "ternvale::mmio", skip_all, fields(device = name))]
    pub fn access_count(&self, name: &str) -> Option<u64> {
        self.devices
            .iter()
            .find(|slot| slot.name.as_ref() == name)
            .map(|slot| slot.accesses.load(Ordering::Relaxed))
    }

    /// Perform one [`ExitEvent::Mmio`] against the matching device.
    ///
    /// Reads store the zero-extended result in `event.reg`. Writes take the
    /// low `size` bytes of that register. An address outside every window
    /// returns 0 on read and is ignored on write.
    ///
    /// Holds only the matching device's lock, and only for the device call.
    pub fn dispatch(&self, regs: &impl GuestRegs, event: ExitEvent) -> Result<(), MmioError> {
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
        if reg > 31 {
            tracing::warn!(target: "ternvale::mmio", reg, "rejected mmio register");
            return Err(MmioError::BadReg { reg });
        }
        let Some(end) = gpa.checked_add(u64::from(size)) else {
            tracing::warn!(target: "ternvale::mmio", gpa, size, "rejected overflowing mmio access");
            return Err(MmioError::Overflow {
                base: gpa,
                size: u64::from(size),
            });
        };
        let Some(index) = self.lookup(gpa, end) else {
            return self.unmapped(regs, gpa, size, write, reg);
        };
        let slot = &self.devices[index];
        let offset = gpa - slot.base;
        #[cfg(feature = "metrics")]
        let started = Instant::now();
        let value = if write {
            let raw = if reg == 31 { 0 } else { regs.get_reg(reg)? };
            let value = mask(raw, size);
            if slot
                .kick
                .as_ref()
                .is_some_and(|kick| kick.try_kick(offset, value))
            {
                value
            } else {
                slot.lock().write(offset, size, value);
                value
            }
        } else {
            let value = mask(slot.lock().read(offset, size), size);
            if reg != 31 {
                regs.set_reg(reg, value)?;
            }
            value
        };
        #[cfg(feature = "metrics")]
        if let (Some(metrics), Some(id)) = (self.metrics.as_ref(), slot.metrics_id) {
            metrics.record_mmio(id, started.elapsed());
        }
        slot.accesses.fetch_add(1, Ordering::Relaxed);
        if let Some(trace) = &self.trace {
            trace.record(slot.trace_id, regs.cpu_id(), write, gpa, size, value);
        }
        if tracing::enabled!(target: "ternvale::mmio", tracing::Level::TRACE) {
            tracing::trace!(
                target: "ternvale::mmio",
                device = slot.name.as_ref(),
                offset = format!("{:#x}", offset),
                size,
                value = format!("{:#x}", value),
                direction = if write { "write" } else { "read" },
                "mmio access"
            );
        }
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
        tracing::warn!(
            target: "ternvale::mmio",
            gpa,
            size,
            write,
            "unmapped mmio access"
        );
        let value = if write {
            if reg == 31 {
                0
            } else {
                mask(regs.get_reg(reg)?, size)
            }
        } else {
            if reg != 31 {
                regs.set_reg(reg, 0)?;
            }
            0
        };
        if let Some(trace) = &self.trace {
            trace.record(None, regs.cpu_id(), write, gpa, size, value);
        }
        if tracing::enabled!(target: "ternvale::mmio", tracing::Level::TRACE) {
            tracing::trace!(
                target: "ternvale::mmio",
                device = "unmapped",
                gpa,
                size,
                value,
                write,
                "mmio access"
            );
        }
        Ok(())
    }
}

impl Drop for MmioBus {
    fn drop(&mut self) {
        for slot in &self.devices {
            tracing::debug!(
                target: "ternvale::mmio",
                device = %slot.name,
                accesses = slot.accesses.load(Ordering::Relaxed),
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
