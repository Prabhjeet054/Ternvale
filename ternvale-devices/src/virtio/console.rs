//! virtio-console (id 3) with MULTIPORT and one named agent port.
//!
//! Spec: OASIS virtio 1.2 §5.3. Port 0 is unused. Port 1 is
//! [`spec::AGENT_PORT_NAME`], spliced to a host Unix socket (the agent server).

mod spec;
mod worker;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ternvale_vmm::{DeviceAttach, GuestMemory, MachineError};

use super::irq::{IrqHook, VirtioIrq};
use super::{QueueNotify, VirtioDevice};

pub use spec::{AGENT_PORT_NAME, F_MULTIPORT, VIRTIO_CONSOLE_ID};

/// Building a virtio-console device failed.
#[derive(Debug, thiserror::Error)]
pub enum ConsoleError {
    /// MMIO slot out of range.
    #[error("virtio-console slot {slot} is outside the virtio-mmio window")]
    BadSlot {
        /// Requested slot.
        slot: u32,
    },
    /// Worker thread could not start.
    #[error("spawn virtio-console worker: {source}")]
    Spawn {
        /// OS error.
        source: std::io::Error,
    },
}

/// Counters for the agent named port.
#[derive(Debug, Default)]
pub struct ConsoleStats {
    /// Bytes guest → host socket.
    pub tx_bytes: AtomicU64,
    /// Bytes host socket → guest.
    pub rx_bytes: AtomicU64,
    /// Times the host socket was connected.
    pub connections: AtomicU64,
}

impl ConsoleStats {
    /// `(tx_bytes, rx_bytes, connections)`.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::console", skip_all)]
    pub fn snapshot(&self) -> [u64; 3] {
        [
            self.tx_bytes.load(Ordering::Relaxed),
            self.rx_bytes.load(Ordering::Relaxed),
            self.connections.load(Ordering::Relaxed),
        ]
    }
}

/// Result of [`VirtioConsole::attach`].
pub type AttachedConsole = (
    u64,
    u64,
    Box<dyn ternvale_vmm::MmioDevice>,
    Arc<ConsoleStats>,
);

/// virtio-console with a dedicated worker.
pub struct VirtioConsole {
    kick: Option<Sender<worker::Kick>>,
    join: Option<JoinHandle<()>>,
    stats: Arc<ConsoleStats>,
}

impl VirtioConsole {
    /// Splice the agent port to `host` (`host-5000.sock`).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::console",
        skip_all,
        fields(host = %host.display())
    )]
    pub fn open(
        host: PathBuf,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
    ) -> Result<Self, ConsoleError> {
        let stats = Arc::new(ConsoleStats::default());
        let state = worker::Worker::new(memory, irq, Arc::clone(&stats), host.clone());
        let (tx, rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("ternvale-virtio-console".into())
            .spawn(move || state.run(rx))
            .map_err(|source| {
                tracing::error!(
                    target: "ternvale::virtio::console",
                    error = %source,
                    "virtio-console worker spawn failed"
                );
                ConsoleError::Spawn { source }
            })?;
        tracing::info!(
            target: "ternvale::virtio::console",
            host = %host.display(),
            port = AGENT_PORT_NAME,
            "virtio-console agent port ready"
        );
        Ok(Self {
            kick: Some(tx),
            join: Some(join),
            stats,
        })
    }

    /// virtio-mmio slot.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::console",
        skip(host, memory, irq_hook),
        fields(slot)
    )]
    pub fn attach(
        slot: u32,
        host: PathBuf,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
    ) -> Result<AttachedConsole, ConsoleError> {
        let base = super::slot_base(slot).ok_or_else(|| {
            tracing::error!(target: "ternvale::virtio::console", slot, "virtio-console slot rejected");
            ConsoleError::BadSlot { slot }
        })?;
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let console = Self::open(host, memory, Arc::clone(&irq))?;
        let stats = Arc::clone(&console.stats);
        let transport = super::mmio::VirtioMmio::with_irq(slot, Box::new(console), irq);
        Ok((
            base,
            ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE,
            Box::new(transport),
            stats,
        ))
    }

    /// Next free virtio-pci function (Windows guests).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::console",
        skip(attach, host)
    )]
    pub fn attach_pci(
        attach: &DeviceAttach,
        host: PathBuf,
    ) -> Result<Arc<ConsoleStats>, MachineError> {
        let irq = VirtioIrq::new();
        let console = Self::open(host, Arc::clone(&attach.memory), Arc::clone(&irq))
            .map_err(|error| MachineError::Attach(error.to_string()))?;
        let stats = Arc::clone(&console.stats);
        let bdf = super::pci::attach_pci(attach, Box::new(console), irq)?;
        tracing::info!(
            target: "ternvale::virtio::console",
            %bdf,
            port = AGENT_PORT_NAME,
            "virtio-console on virtio-pci"
        );
        Ok(stats)
    }

    fn send(&self, kick: worker::Kick) {
        let Some(tx) = self.kick.as_ref() else {
            return;
        };
        if let Err(error) = tx.send(kick) {
            tracing::error!(
                target: "ternvale::virtio::console",
                error = %error,
                "virtio-console kick failed"
            );
        }
    }
}

impl Drop for VirtioConsole {
    fn drop(&mut self) {
        self.kick.take();
        if let Some(join) = self.join.take() {
            if let Err(error) = join.join() {
                tracing::error!(
                    target: "ternvale::virtio::console",
                    ?error,
                    "virtio-console worker panicked"
                );
            }
        }
    }
}

impl VirtioDevice for VirtioConsole {
    fn device_id(&self) -> u32 {
        VIRTIO_CONSOLE_ID
    }

    fn device_features(&self) -> u64 {
        F_MULTIPORT
    }

    fn num_queues(&self) -> u16 {
        spec::NUM_QUEUES
    }

    fn read_config(&self, offset: u64, size: u8) -> u64 {
        // cols/rows at 0, max_nr_ports at 4 (le32). Spec 5.3.1.
        let mut cfg = [0u8; 8];
        cfg[4..8].copy_from_slice(&spec::MAX_PORTS.to_le_bytes());
        let start = offset as usize;
        let end = start.saturating_add(size as usize);
        if end > cfg.len() {
            return 0;
        }
        let mut val = 0u64;
        for (i, b) in cfg[start..end].iter().enumerate() {
            val |= u64::from(*b) << (8 * i);
        }
        val
    }

    fn notify(&self, notify: QueueNotify) {
        self.send(worker::Kick::Notify(notify));
    }

    fn status_changed(&self, status: u32) {
        if status == 0 {
            self.send(worker::Kick::Reset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_space_reports_two_ports() {
        let mem = Arc::new(Mutex::new(GuestMemory::new().expect("mem")));
        let irq = VirtioIrq::new();
        let console = VirtioConsole::open("/tmp/none.sock".into(), mem, irq).expect("open");
        assert_eq!(console.device_id(), VIRTIO_CONSOLE_ID);
        assert_eq!(console.device_features(), F_MULTIPORT);
        assert_eq!(console.read_config(4, 4), u64::from(spec::MAX_PORTS));
    }
}
