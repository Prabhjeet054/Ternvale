//! virtio-sound (id 25): guest PCM on TX → CoreAudio; optional RX capture.

mod protocol;
mod worker;

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ternvale_vmm::GuestMemory;

use self::protocol::VIRTIO_SND_ID;

use super::irq::{IrqHook, VirtioIrq};
use super::{QueueNotify, VirtioDevice, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};
pub use protocol::parse_set_params as parse_sound_set_params;
pub use protocol::VIRTIO_SND_ID as VIRTIO_SOUND_ID;

/// Building or attaching virtio-sound failed.
#[derive(Debug, thiserror::Error)]
pub enum SoundError {
    /// MMIO slot is out of range.
    #[error("virtio-sound slot {slot} is outside the virtio-mmio window")]
    BadSlot {
        /// Requested slot.
        slot: u32,
    },
    /// Worker thread failed to start.
    #[error("spawn virtio-sound worker: {source}")]
    Spawn {
        /// OS error.
        source: std::io::Error,
    },
}

/// Base, size, MMIO device from [`VirtioSound::attach`].
pub type AttachedSound = (u64, u64, Box<dyn ternvale_vmm::MmioDevice>);

/// virtio-sound with a dedicated worker (playback, optional capture).
pub struct VirtioSound {
    config: [u8; 12],
    kick: Option<Sender<worker::Kick>>,
    join: Option<JoinHandle<()>>,
    capturing: Arc<AtomicBool>,
}

impl VirtioSound {
    /// Start the worker (playback only).
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::sound", skip(memory, irq))]
    pub fn open(memory: Arc<Mutex<GuestMemory>>, irq: Arc<VirtioIrq>) -> Result<Self, SoundError> {
        Self::open_with_capture(memory, irq, false, Arc::new(AtomicBool::new(false)))
    }

    /// Start the worker. `capture` advertises an input stream (host microphone).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::sound",
        skip(memory, irq, capturing),
        fields(capture)
    )]
    pub fn open_with_capture(
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        capture: bool,
        capturing: Arc<AtomicBool>,
    ) -> Result<Self, SoundError> {
        let streams = if capture { 2u32 } else { 1u32 };
        let mut config = [0u8; 12];
        config[4..8].copy_from_slice(&streams.to_le_bytes());
        let (tx, rx) = mpsc::channel();
        let worker = worker::Worker::new(memory, irq, capture, Arc::clone(&capturing));
        let join = std::thread::Builder::new()
            .name("ternvale-virtio-sound".into())
            .spawn(move || worker.run(rx))
            .map_err(|source| SoundError::Spawn { source })?;
        tracing::info!(
            target: "ternvale::virtio::sound",
            streams,
            capture,
            "virtio-sound device ready"
        );
        Ok(Self {
            config,
            kick: Some(tx),
            join: Some(join),
            capturing,
        })
    }

    /// Host microphone is feeding RX.
    pub fn capturing(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.capturing)
    }

    /// Attach on virtio-mmio `slot` (playback only).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::sound",
        skip(memory, irq_hook),
        fields(slot)
    )]
    pub fn attach(
        slot: u32,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
    ) -> Result<AttachedSound, SoundError> {
        Self::attach_capture(
            slot,
            memory,
            irq_hook,
            false,
            Arc::new(AtomicBool::new(false)),
        )
    }

    /// Attach playback, and a capture stream when `capture` is true.
    ///
    /// `capturing` is set while the host input callback is running (no PCM).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::sound",
        skip(memory, irq_hook, capturing),
        fields(slot, capture)
    )]
    pub fn attach_capture(
        slot: u32,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
        capture: bool,
        capturing: Arc<AtomicBool>,
    ) -> Result<AttachedSound, SoundError> {
        let base = super::slot_base(slot).ok_or(SoundError::BadSlot { slot })?;
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let sound = Self::open_with_capture(memory, Arc::clone(&irq), capture, capturing)?;
        let transport = super::mmio::VirtioMmio::with_irq(slot, Box::new(sound), irq);
        Ok((
            base,
            ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE,
            Box::new(transport),
        ))
    }

    fn send(&self, kick: worker::Kick) {
        if let Some(tx) = self.kick.as_ref() {
            if let Err(err) = tx.send(kick) {
                tracing::error!(
                    target: "ternvale::virtio::sound",
                    error = %err,
                    "virtio-sound kick failed"
                );
            }
        }
    }
}

impl Drop for VirtioSound {
    fn drop(&mut self) {
        if let Some(tx) = self.kick.as_ref() {
            let _ = tx.send(worker::Kick::Reset);
        }
        self.kick.take();
        if let Some(join) = self.join.take() {
            if let Err(err) = join.join() {
                tracing::error!(
                    target: "ternvale::virtio::sound",
                    ?err,
                    "virtio-sound worker panicked"
                );
            }
        }
    }
}

impl VirtioDevice for VirtioSound {
    fn device_id(&self) -> u32 {
        VIRTIO_SND_ID
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_INDIRECT_DESC | VIRTIO_F_EVENT_IDX
    }

    fn num_queues(&self) -> u16 {
        4
    }

    fn queue_num_max(&self, _queue: u16) -> u16 {
        256
    }

    fn read_config(&self, offset: u64, size: u8) -> u64 {
        read_le(&self.config, offset, size)
    }

    fn notify(&self, queue: QueueNotify) {
        self.send(worker::Kick::Notify(queue));
    }

    fn status_changed(&self, status: u32) {
        if status == 0 {
            self.send(worker::Kick::Reset);
        }
    }
}

fn read_le(bytes: &[u8], offset: u64, size: u8) -> u64 {
    let Ok(start) = usize::try_from(offset) else {
        return 0;
    };
    if start >= bytes.len() {
        return 0;
    }
    let mut tmp = [0u8; 8];
    let n = usize::from(size).min(8).min(bytes.len() - start);
    tmp[..n].copy_from_slice(&bytes[start..start + n]);
    u64::from_le_bytes(tmp)
}

#[cfg(test)]
#[path = "sound_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "sound_quality.rs"]
mod quality;
