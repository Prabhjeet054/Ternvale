//! GICv3 redistributor accesses the in-kernel GIC does not answer on its own.
//!
//! Linux checks `GICR_PIDR2` at offset `0xFFE8` before it trusts `GICR_TYPER`.
//! Apple's framework leaves both for the VMM. Each CPU walks the 128 KiB
//! frames (RD + SGI) reading `GICR_TYPER` until the affinity matches its own
//! MPIDR, stopping at the frame with `Last` set. Which frame belongs to which
//! vCPU comes from `hv_gic_get_redistributor_base`, recorded per vCPU in
//! [`RedistMap`]. Other redistributor writes are dropped:
//! `hv_gic_set_redistributor_reg` returns `HV_DENIED` once the guest runs.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::mmio::MmioDevice;
use crate::platform::{GIC_REDIST_BASE, GIC_REDIST_SIZE};
use crate::smp::{gicr_affinity, mpidr};

/// `GICR_PIDR2.ArchRev` value for GICv3.
const PIDR2_GICV3: u64 = 0x30;
const PIDR2_OFFSET: u64 = 0xffe8;
const TYPER_OFFSET: u64 = 0x8;
const TYPER_LAST: u64 = 1 << 4;
/// RD_base plus SGI_base, 64 KiB each (no VLPI frames).
pub const REDIST_STRIDE: u64 = 0x2_0000;
const FRAMES: usize = (GIC_REDIST_SIZE / REDIST_STRIDE) as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
    cpu: u32,
    mpidr: u64,
}

/// Which redistributor frame belongs to which guest CPU.
#[derive(Debug)]
pub struct RedistMap {
    frames: Mutex<Vec<Option<Frame>>>,
}

impl RedistMap {
    /// Frame `n` is CPU `n` until [`RedistMap::record`] says otherwise.
    #[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all, fields(cpus))]
    pub fn new(cpus: u32) -> Arc<Self> {
        let mut frames = vec![None; FRAMES];
        for cpu in 0..cpus.min(FRAMES as u32) {
            frames[cpu as usize] = Some(Frame {
                cpu,
                mpidr: mpidr(cpu),
            });
        }
        Arc::new(Self {
            frames: Mutex::new(frames),
        })
    }

    /// CPU `cpu` (MPIDR `mpidr`) was given the redistributor at GPA `base`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gic",
        skip_all,
        fields(cpu, mpidr = format!("{mpidr:#x}"), base = format!("{base:#x}"))
    )]
    pub fn record(&self, cpu: u32, mpidr: u64, base: u64) {
        let offset = base.wrapping_sub(GIC_REDIST_BASE);
        let index = (offset / REDIST_STRIDE) as usize;
        if base < GIC_REDIST_BASE || offset % REDIST_STRIDE != 0 || index >= FRAMES {
            tracing::warn!(
                target: "ternvale::gic",
                cpu,
                base = format!("{base:#x}"),
                "framework redistributor base is outside the guest window; keeping frame {cpu}"
            );
            return;
        }
        let mut frames = self.lock();
        let frame = Frame { cpu, mpidr };
        if frames[index] == Some(frame) {
            tracing::debug!(target: "ternvale::gic", cpu, frame = index, "redistributor frame matches the default");
            return;
        }
        for slot in frames.iter_mut() {
            if slot.is_some_and(|other| other.cpu == cpu) {
                *slot = None;
            }
        }
        if let Some(evicted) = frames[index].replace(frame) {
            tracing::warn!(target: "ternvale::gic", cpu, evicted = evicted.cpu, frame = index, "redistributor frame reassigned");
        } else {
            tracing::info!(target: "ternvale::gic", cpu, frame = index, "redistributor frame moved");
        }
    }

    /// `GICR_TYPER` for frame `index`. Zero for a frame no CPU owns.
    #[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all, fields(frame = index))]
    pub fn typer(&self, index: usize) -> u64 {
        let frames = self.lock();
        let Some(Some(frame)) = frames.get(index).copied() else {
            return 0;
        };
        let last = frames.iter().rposition(Option::is_some) == Some(index);
        (gicr_affinity(frame.mpidr) << 32)
            | (u64::from(frame.cpu) << 8)
            | if last { TYPER_LAST } else { 0 }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Option<Frame>>> {
        crate::lockwatch::lock(&self.frames, "redist-map")
    }
}

/// Redistributor identification registers.
pub struct RedistId {
    map: Arc<RedistMap>,
}

impl RedistId {
    /// Answer `GICR_TYPER` from `map`.
    #[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all)]
    pub fn new(map: Arc<RedistMap>) -> Self {
        Self { map }
    }
}

impl MmioDevice for RedistId {
    fn name(&self) -> &str {
        "gic-redist"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        let index = (offset / REDIST_STRIDE) as usize;
        let within = offset % REDIST_STRIDE;
        let value = if within & 0xffff == PIDR2_OFFSET {
            PIDR2_GICV3
        } else if within == TYPER_OFFSET {
            self.map.typer(index)
        } else if within == TYPER_OFFSET + 4 && size == 4 {
            self.map.typer(index) >> 32
        } else {
            0
        };
        if value != 0 {
            tracing::trace!(
                target: "ternvale::gic",
                offset = format!("{:#x}", offset),
                frame = index,
                value = format!("{:#x}", value),
                "gic redistributor read"
            );
            return value;
        }
        tracing::debug!(
            target: "ternvale::gic",
            offset = format!("{:#x}", offset),
            frame = index,
            "gic redistributor read not claimed by the framework"
        );
        0
    }

    fn write(&mut self, offset: u64, _size: u8, value: u64) {
        tracing::debug!(
            target: "ternvale::gic",
            offset = format!("{:#x}", offset),
            value = format!("{:#x}", value),
            "gic redistributor write not claimed by the framework"
        );
    }
}

#[cfg(test)]
#[path = "gic_redist_tests.rs"]
mod tests;
