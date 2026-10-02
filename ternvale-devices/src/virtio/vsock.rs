//! virtio-vsock device (id 19): stream sockets between the guest and host Unix sockets.
//!
//! Queues are RX (0), TX (1), and event (2). Config space is the 64-bit guest
//! CID. Only `VIRTIO_VSOCK_F_STREAM` is offered; SEQPACKET requests get RST.
//! See [`host_socket_path`] / [`guest_socket_path`] for the per-port sockets.

mod cid;
mod conn;
mod host;
mod muxer;
mod packet;
mod worker;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ternvale_vmm::GuestMemory;

use super::irq::{IrqHook, VirtioIrq};
use super::{QueueNotify, VirtioDevice};

pub use cid::{CidLease, FIRST_GUEST_CID};
pub use host::{guest_socket_path, host_socket_path};
pub use packet::{op_name, Header as VsockHeader, HDR_LEN as VSOCK_HDR_LEN};

/// Virtio socket device id.
pub const VIRTIO_VSOCK_ID: u32 = 19;
/// `VIRTIO_VSOCK_F_STREAM` (bit 0). Spec 1.2 says a device with no negotiated
/// bits acts as if this one were negotiated, so older drivers still get streams.
/// Linux 6.6 does not acknowledge this bit (it accepts only `VERSION_1`), and
/// stream sockets still work.
pub const VIRTIO_VSOCK_F_STREAM: u64 = 1 << 0;
/// `VMADDR_CID_HOST`.
pub const HOST_CID: u64 = 2;

pub(super) const RX_QUEUE: u16 = 0;
pub(super) const TX_QUEUE: u16 = 1;
pub(super) const EVENT_QUEUE: u16 = 2;

/// Building a virtio-vsock device failed.
#[derive(Debug, thiserror::Error)]
pub enum VsockError {
    /// The worker thread could not be started.
    #[error("spawn virtio-vsock worker: {source}")]
    Spawn {
        /// OS error.
        source: std::io::Error,
    },
    /// The virtio-mmio slot is outside the platform window.
    #[error("virtio-vsock slot {slot} is outside the virtio-mmio window")]
    BadSlot {
        /// Requested slot.
        slot: u32,
    },
    /// The CID is reserved (0, 1, 2, or `VMADDR_CID_ANY`).
    #[error("guest cid {cid} is reserved; use 3 or higher")]
    BadCid {
        /// Requested CID.
        cid: u32,
    },
    /// Another device in this process holds the CID.
    #[error("guest cid {cid} is already in use")]
    CidInUse {
        /// Requested CID.
        cid: u32,
    },
    /// Every guest CID is leased.
    #[error("no free guest cid")]
    NoFreeCid,
    /// A host Unix socket or its directory could not be set up.
    #[error("vsock unix socket {}: {source}", path.display())]
    Uds {
        /// Socket or directory path.
        path: PathBuf,
        /// OS error.
        source: std::io::Error,
    },
}

/// How to build a vsock device.
#[derive(Debug, Clone)]
pub struct VsockConfig {
    /// Guest CID, or `None` for the lowest free CID from 3.
    pub guest_cid: Option<u32>,
    /// Directory for the per-port Unix sockets. Keep it short: macOS limits
    /// socket paths to 104 bytes.
    pub uds_dir: PathBuf,
    /// Guest ports host processes may connect to (one `guest-<port>.sock` each).
    pub listen_ports: Vec<u32>,
}

/// Base, size, MMIO device, live stats, and guest CID from [`VirtioVsock::attach`].
pub type AttachedVsock = (
    u64,
    u64,
    Box<dyn ternvale_vmm::MmioDevice>,
    Arc<VsockStats>,
    u32,
);

/// Counters for one vsock device.
#[derive(Debug, Default)]
pub struct VsockStats {
    /// Packets taken from the TX queue.
    pub tx_packets: AtomicU64,
    /// Payload bytes in those packets.
    pub tx_bytes: AtomicU64,
    /// Packets written to the RX queue.
    pub rx_packets: AtomicU64,
    /// Payload bytes in those packets.
    pub rx_bytes: AtomicU64,
    /// Connections opened in either direction.
    pub connections: AtomicU64,
    /// RST packets sent to the guest.
    pub resets: AtomicU64,
    /// Malformed or misaddressed packets and unusable buffers.
    pub dropped: AtomicU64,
}

/// Point-in-time copy of [`VsockStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsockCounters {
    /// See [`VsockStats::tx_packets`].
    pub tx_packets: u64,
    /// See [`VsockStats::tx_bytes`].
    pub tx_bytes: u64,
    /// See [`VsockStats::rx_packets`].
    pub rx_packets: u64,
    /// See [`VsockStats::rx_bytes`].
    pub rx_bytes: u64,
    /// See [`VsockStats::connections`].
    pub connections: u64,
    /// See [`VsockStats::resets`].
    pub resets: u64,
    /// See [`VsockStats::dropped`].
    pub dropped: u64,
}

impl VsockStats {
    /// Copy every counter.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::vsock", skip_all)]
    pub fn snapshot(&self) -> VsockCounters {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        VsockCounters {
            tx_packets: get(&self.tx_packets),
            tx_bytes: get(&self.tx_bytes),
            rx_packets: get(&self.rx_packets),
            rx_bytes: get(&self.rx_bytes),
            connections: get(&self.connections),
            resets: get(&self.resets),
            dropped: get(&self.dropped),
        }
    }
}

/// One virtio-vsock device with a dedicated worker thread.
pub struct VirtioVsock {
    lease: CidLease,
    kick: Option<Sender<worker::Kick>>,
    join: Option<JoinHandle<()>>,
    stats: Arc<VsockStats>,
}

impl VirtioVsock {
    /// Lease a CID, bind the host listeners, and start the worker.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::vsock",
        skip_all,
        fields(uds_dir = %config.uds_dir.display(), ports = ?config.listen_ports)
    )]
    pub fn open(
        config: &VsockConfig,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
    ) -> Result<Self, VsockError> {
        let lease = CidLease::acquire(config.guest_cid)?;
        let host = host::HostSide::open(&config.uds_dir, &config.listen_ports)?;
        let stats = Arc::new(VsockStats::default());
        let muxer = muxer::Muxer::new(lease.cid(), host, Arc::clone(&stats));
        let state = worker::Worker::new(muxer, memory, irq, Arc::clone(&stats));
        let (tx, rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("ternvale-virtio-vsock".into())
            .spawn(move || state.run(rx))
            .map_err(|source| {
                tracing::error!(
                    target: "ternvale::virtio::vsock",
                    error = %source,
                    "virtio-vsock worker spawn failed"
                );
                VsockError::Spawn { source }
            })?;
        tracing::info!(
            target: "ternvale::virtio::vsock",
            guest_cid = lease.cid(),
            uds_dir = %config.uds_dir.display(),
            "virtio-vsock device ready"
        );
        Ok(Self {
            lease,
            kick: Some(tx),
            join: Some(join),
            stats,
        })
    }

    /// Build a transport on `slot`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::vsock",
        skip(config, memory, irq_hook),
        fields(slot)
    )]
    pub fn attach(
        slot: u32,
        config: &VsockConfig,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
    ) -> Result<AttachedVsock, VsockError> {
        let base = super::slot_base(slot).ok_or_else(|| {
            tracing::error!(target: "ternvale::virtio::vsock", slot, "virtio-vsock slot rejected");
            VsockError::BadSlot { slot }
        })?;
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let vsock = Self::open(config, memory, Arc::clone(&irq))?;
        let stats = vsock.stats();
        let cid = vsock.guest_cid();
        let transport = super::mmio::VirtioMmio::with_irq(slot, Box::new(vsock), irq);
        Ok((
            base,
            ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE,
            Box::new(transport),
            stats,
            cid,
        ))
    }

    /// CID the guest sees in config space.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::vsock", skip_all)]
    pub fn guest_cid(&self) -> u32 {
        self.lease.cid()
    }

    /// Live counters.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::vsock", skip_all)]
    pub fn stats(&self) -> Arc<VsockStats> {
        Arc::clone(&self.stats)
    }

    fn send(&self, kick: worker::Kick) {
        let Some(tx) = self.kick.as_ref() else {
            return;
        };
        if let Err(error) = tx.send(kick) {
            tracing::error!(
                target: "ternvale::virtio::vsock",
                error = %error,
                "virtio-vsock kick failed"
            );
        }
    }
}

impl Drop for VirtioVsock {
    fn drop(&mut self) {
        self.kick.take();
        if let Some(join) = self.join.take() {
            if let Err(error) = join.join() {
                tracing::error!(
                    target: "ternvale::virtio::vsock",
                    ?error,
                    "virtio-vsock worker panicked"
                );
            }
        }
    }
}

impl VirtioDevice for VirtioVsock {
    fn device_id(&self) -> u32 {
        VIRTIO_VSOCK_ID
    }

    fn device_features(&self) -> u64 {
        VIRTIO_VSOCK_F_STREAM
    }

    fn num_queues(&self) -> u16 {
        3
    }

    fn read_config(&mut self, offset: u64, size: u8) -> u64 {
        let bytes = u64::from(self.lease.cid()).to_le_bytes();
        let value = match usize::try_from(offset) {
            Ok(start) if start < bytes.len() => {
                let mut tmp = [0u8; 8];
                let n = usize::from(size).min(8).min(bytes.len() - start);
                tmp[..n].copy_from_slice(&bytes[start..start + n]);
                u64::from_le_bytes(tmp)
            }
            _ => 0,
        };
        tracing::trace!(
            target: "ternvale::virtio::vsock",
            offset,
            size,
            value = %format!("{value:#x}"),
            "virtio-vsock config read"
        );
        value
    }

    fn write_config(&mut self, offset: u64, size: u8, value: u64) {
        tracing::warn!(
            target: "ternvale::virtio::vsock",
            offset,
            size,
            value = %format!("{value:#x}"),
            "ignored virtio-vsock config write"
        );
    }

    fn notify(&mut self, queue: QueueNotify) {
        self.send(worker::Kick::Notify(queue));
    }

    fn status_changed(&mut self, status: u32) {
        if status == 0 {
            self.send(worker::Kick::Reset);
        }
    }
}

#[cfg(test)]
#[path = "vsock_tests.rs"]
mod tests;
