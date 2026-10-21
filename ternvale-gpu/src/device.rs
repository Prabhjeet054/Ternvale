//! `VirtioGpu`: the [`VirtioDevice`] the shared transports drive, plus its
//! virtio-mmio and virtio-pci attach paths.
//!
//! Device id 16, two queues (`controlq`, `cursorq`), no feature bits (the
//! 2D-only profile: no `VIRGL`, `EDID`, `RESOURCE_UUID`, `RESOURCE_BLOB` or
//! `CONTEXT_INIT`). Config space is `struct virtio_gpu_config` (virtio 1.2
//! §5.7.4): `events_read`, `events_clear`, `num_scanouts`, `num_capsets`.

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ternvale_devices::{IrqHook, QueueNotify, VirtioDevice, VirtioIrq, VirtioMmio};
use ternvale_vmm::pci::Bdf;
use ternvale_vmm::{DeviceAttach, GuestMemory, MachineError, MmioDevice};

use crate::engine::GpuEngine;
use crate::gpu2d::{GpuStats, MAX_DIMENSION};
#[cfg(feature = "gpu3d")]
use crate::gpu3d::CAPSET_COUNT;
use crate::protocol::MAX_SCANOUTS;
use crate::scanout::GpuDisplay;
use crate::worker::{self, Kick};

/// `VIRTIO_ID_GPU`.
pub const VIRTIO_GPU_ID: u32 = 16;
/// `sizeof(struct virtio_gpu_config)` without `blob_alignment` (2D-only).
pub const CONFIG_LEN: u64 = 16;
/// Full `virtio_gpu_config` including `blob_alignment` (`gpu3d`).
#[cfg(feature = "gpu3d")]
pub const CONFIG_LEN_3D: u64 = 20;
/// Page alignment for blob resources (`gpu3d`).
#[cfg(feature = "gpu3d")]
pub const BLOB_ALIGNMENT: u32 = 4096;

/// `VIRTIO_GPU_F_VIRGL` (bit 0).
#[cfg(feature = "gpu3d")]
pub const F_VIRGL: u64 = 1 << 0;
/// `VIRTIO_GPU_F_RESOURCE_BLOB` (bit 3).
#[cfg(feature = "gpu3d")]
pub const F_RESOURCE_BLOB: u64 = 1 << 3;
/// `VIRTIO_GPU_F_CONTEXT_INIT` (bit 4).
#[cfg(feature = "gpu3d")]
pub const F_CONTEXT_INIT: u64 = 1 << 4;
/// `VIRTIO_GPU_F_BLOB_ALIGNMENT` (bit 5).
#[cfg(feature = "gpu3d")]
pub const F_BLOB_ALIGNMENT: u64 = 1 << 5;
/// Mode a scanout reports when the config does not choose one.
pub const DEFAULT_MODE: (u32, u32) = (1280, 800);

/// Display configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuConfig {
    /// (width, height) of each scanout, 1 to 16 of them.
    pub modes: Vec<(u32, u32)>,
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            modes: vec![DEFAULT_MODE],
        }
    }
}

impl GpuConfig {
    /// Check scanout count and sizes.
    pub fn validate(&self) -> Result<(), GpuError> {
        if self.modes.is_empty() || self.modes.len() > MAX_SCANOUTS {
            return Err(GpuError::BadConfig(format!(
                "{} scanouts; need 1 to {MAX_SCANOUTS}",
                self.modes.len()
            )));
        }
        for (index, &(width, height)) in self.modes.iter().enumerate() {
            if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
                return Err(GpuError::BadConfig(format!(
                    "scanout {index} mode {width}x{height} outside 1..={MAX_DIMENSION}"
                )));
            }
        }
        Ok(())
    }
}

/// Building a virtio-gpu device failed.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// The display configuration is invalid.
    #[error("virtio-gpu config: {0}")]
    BadConfig(String),
    /// The virtio-mmio slot is outside the platform window.
    #[error("virtio-gpu slot {slot} is outside the virtio-mmio window")]
    BadSlot {
        /// Requested slot.
        slot: u32,
    },
    /// The worker thread could not start.
    #[error("spawn virtio-gpu worker: {0}")]
    Spawn(#[source] std::io::Error),
}

/// A virtio-gpu on a virtio-mmio slot: what the attach callback returns, plus handles.
pub struct AttachedGpu {
    /// MMIO base of the slot.
    pub base: u64,
    /// MMIO window size.
    pub size: u64,
    /// Transport to put on the bus.
    pub device: Box<dyn MmioDevice>,
    /// Scanout frames.
    pub display: GpuDisplay,
    /// Live counters.
    pub stats: Arc<GpuStats>,
}

/// A virtio-gpu on a PCI function.
pub struct PciGpu {
    /// Bus address.
    pub bdf: Bdf,
    /// Scanout frames.
    pub display: GpuDisplay,
    /// Live counters.
    pub stats: Arc<GpuStats>,
}

/// One virtio-gpu device with its worker thread.
pub struct VirtioGpu {
    config: GpuConfig,
    events_read: u32,
    display: GpuDisplay,
    stats: Arc<GpuStats>,
    kick: Option<Sender<Kick>>,
    join: Option<JoinHandle<()>>,
}

impl VirtioGpu {
    /// Device over `memory` raising `irq` on completions. Starts the worker.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(scanouts = config.modes.len()))]
    pub fn new(
        config: GpuConfig,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
    ) -> Result<Self, GpuError> {
        config.validate()?;
        let display = GpuDisplay::new(&config.modes);
        let stats = Arc::new(GpuStats::default());
        let gpu = GpuEngine::new(display.clone(), Arc::clone(&stats));
        let (tx, rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("ternvale-virtio-gpu".into())
            .spawn(move || worker::run(rx, gpu, memory, irq))
            .map_err(GpuError::Spawn)?;
        tracing::info!(
            target: "ternvale::gpu",
            scanouts = config.modes.len(),
            modes = ?config.modes,
            gpu3d = cfg!(feature = "gpu3d"),
            "virtio-gpu device ready (no cursor)"
        );
        Ok(Self {
            config,
            events_read: 0,
            display,
            stats,
            kick: Some(tx),
            join: Some(join),
        })
    }

    /// Scanout frames.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn display(&self) -> GpuDisplay {
        self.display.clone()
    }

    /// Live counters.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn stats(&self) -> Arc<GpuStats> {
        Arc::clone(&self.stats)
    }

    /// Build a virtio-mmio transport on `slot`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu",
        skip(config, memory, irq_hook),
        fields(slot)
    )]
    pub fn attach(
        slot: u32,
        config: GpuConfig,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
    ) -> Result<AttachedGpu, GpuError> {
        let base = ternvale_devices::slot_base(slot).ok_or_else(|| {
            tracing::error!(target: "ternvale::gpu", slot, "virtio-gpu slot rejected");
            GpuError::BadSlot { slot }
        })?;
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let gpu = Self::new(config, memory, Arc::clone(&irq))?;
        let (display, stats) = (gpu.display(), gpu.stats());
        tracing::info!(target: "ternvale::gpu", slot, base = format!("{base:#x}"), "virtio-gpu on virtio-mmio");
        Ok(AttachedGpu {
            base,
            size: ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE,
            device: Box::new(VirtioMmio::with_irq(slot, Box::new(gpu), irq)),
            display,
            stats,
        })
    }

    /// Put a virtio-gpu on the next free virtio-pci function of bus 0.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn attach_pci(attach: &DeviceAttach, config: GpuConfig) -> Result<PciGpu, MachineError> {
        let irq = VirtioIrq::new();
        let gpu = Self::new(config, Arc::clone(&attach.memory), Arc::clone(&irq))
            .map_err(|error| MachineError::Attach(format!("virtio-gpu: {error}")))?;
        let (display, stats) = (gpu.display(), gpu.stats());
        let bdf = ternvale_devices::attach_pci(attach, Box::new(gpu), irq)?;
        tracing::info!(target: "ternvale::gpu", %bdf, "virtio-gpu on virtio-pci");
        Ok(PciGpu {
            bdf,
            display,
            stats,
        })
    }

    fn send(&self, kick: Kick) {
        let Some(tx) = self.kick.as_ref() else {
            return;
        };
        if let Err(error) = tx.send(kick) {
            tracing::error!(target: "ternvale::gpu", error = %error, "virtio-gpu worker gone; kick dropped");
        }
    }

    fn config_len(&self) -> u64 {
        #[cfg(feature = "gpu3d")]
        {
            CONFIG_LEN_3D
        }
        #[cfg(not(feature = "gpu3d"))]
        {
            CONFIG_LEN
        }
    }

    fn config_bytes(&self) -> Vec<u8> {
        let len = self.config_len() as usize;
        let mut out = vec![0u8; len];
        out[0..4].copy_from_slice(&self.events_read.to_le_bytes());
        // events_clear (4..8) reads as 0.
        out[8..12].copy_from_slice(&(self.config.modes.len() as u32).to_le_bytes());
        #[cfg(feature = "gpu3d")]
        {
            out[12..16].copy_from_slice(&CAPSET_COUNT.to_le_bytes());
            out[16..20].copy_from_slice(&BLOB_ALIGNMENT.to_le_bytes());
        }
        #[cfg(not(feature = "gpu3d"))]
        {
            // num_capsets (12..16) is 0: no 3D.
        }
        out
    }
}

impl Drop for VirtioGpu {
    fn drop(&mut self) {
        self.kick.take();
        if let Some(join) = self.join.take() {
            if let Err(error) = join.join() {
                tracing::error!(target: "ternvale::gpu", ?error, "virtio-gpu worker panicked");
            }
        }
    }
}

impl VirtioDevice for VirtioGpu {
    fn device_id(&self) -> u32 {
        VIRTIO_GPU_ID
    }

    fn device_features(&self) -> u64 {
        #[cfg(feature = "gpu3d")]
        {
            F_VIRGL | F_RESOURCE_BLOB | F_CONTEXT_INIT | F_BLOB_ALIGNMENT
        }
        #[cfg(not(feature = "gpu3d"))]
        {
            0
        }
    }

    fn num_queues(&self) -> u16 {
        2
    }

    fn read_config(&mut self, offset: u64, size: u8) -> u64 {
        let bytes = self.config_bytes();
        let config_len = self.config_len();
        let value = match offset.checked_add(u64::from(size)) {
            Some(end) if end <= config_len && matches!(size, 1 | 2 | 4 | 8) => {
                let mut word = [0u8; 8];
                word[..usize::from(size)].copy_from_slice(&bytes[offset as usize..end as usize]);
                u64::from_le_bytes(word)
            }
            _ => {
                tracing::warn!(target: "ternvale::gpu", offset = format!("{offset:#x}"), size, "virtio-gpu config read out of range");
                0
            }
        };
        tracing::trace!(target: "ternvale::gpu", offset = format!("{offset:#x}"), size, value = format!("{value:#x}"), "virtio-gpu config read");
        value
    }

    fn write_config(&mut self, offset: u64, size: u8, value: u64) {
        tracing::trace!(target: "ternvale::gpu", offset = format!("{offset:#x}"), size, value = format!("{value:#x}"), "virtio-gpu config write");
        if offset == 4 && size == 4 {
            // events_clear: write-to-clear bits of events_read (§5.7.4).
            self.events_read &= !(value as u32);
            tracing::debug!(target: "ternvale::gpu", cleared = format!("{value:#x}"), events_read = self.events_read, "virtio-gpu events cleared");
        } else {
            tracing::warn!(target: "ternvale::gpu", offset = format!("{offset:#x}"), size, "write to read-only virtio-gpu config field ignored");
        }
    }

    fn notify(&mut self, queue: QueueNotify) {
        self.send(Kick::Notify(queue));
    }

    fn status_changed(&mut self, status: u32) {
        if status == 0 {
            self.events_read = 0;
            self.send(Kick::Reset);
        }
    }
}

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
