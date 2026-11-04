//! virtio-net device: one RX and one TX queue over a pluggable [`NetBackend`].
//!
//! Offered features are `VIRTIO_NET_F_MAC` and `VIRTIO_NET_F_STATUS`.
//! `VIRTIO_NET_F_MRG_RXBUF`, checksum offload, and GSO are not offered, so every
//! RX packet fits one chain and every header carries `num_buffers = 1`.

mod backend;
mod loopback;
mod packet;
mod pcap;
#[cfg(feature = "vmnet")]
mod vmnet;
mod worker;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ternvale_vmm::GuestMemory;

use super::irq::{IrqHook, VirtioIrq};
use super::{QueueNotify, VirtioDevice};
use ternvale_metrics::Metrics;

pub use backend::{open_backend, open_nic, NetBackend};
pub use loopback::{LoopbackBackend, GATEWAY_IP, GATEWAY_MAC};
pub use packet::{summarize, Kind as PacketKind, Summary as PacketSummary};
pub use pcap::{PcapWriter, PCAP_ENV};
#[cfg(feature = "vmnet")]
pub use vmnet::{VmnetBackend, VMNET_SHARED_MODE};

/// Virtio network device id.
pub const VIRTIO_NET_ID: u32 = 1;
/// Device has a MAC address in config space.
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;
/// Driver can merge receive buffers. Not offered by Ternvale yet.
pub const VIRTIO_NET_F_MRG_RXBUF: u64 = 1 << 15;
/// Config space carries a link status word.
pub const VIRTIO_NET_F_STATUS: u64 = 1 << 16;
/// Link-up bit in the config status word.
pub const VIRTIO_NET_S_LINK_UP: u16 = 1;
/// `struct virtio_net_hdr` size once `VIRTIO_F_VERSION_1` is negotiated.
pub const NET_HDR_LEN: usize = 12;
/// Largest Ethernet frame (without FCS) the device moves in either direction.
pub const MAX_FRAME: usize = 65_535;
/// Locally administered MAC used when the caller has no preference.
pub const DEFAULT_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

pub(super) const RX_QUEUE: u16 = 0;
pub(super) const TX_QUEUE: u16 = 1;

/// Building a virtio-net device or backend failed.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// The worker thread could not be started.
    #[error("spawn virtio-net worker: {source}")]
    Spawn {
        /// OS error.
        source: std::io::Error,
    },
    /// The virtio-mmio slot is outside the platform window.
    #[error("virtio-net slot {slot} is outside the virtio-mmio window")]
    BadSlot {
        /// Requested slot.
        slot: u32,
    },
    /// The pcap file could not be created or written.
    #[error("pcap {}: {source}", path.display())]
    Pcap {
        /// Capture path.
        path: PathBuf,
        /// OS error.
        source: std::io::Error,
    },
    /// No backend has this name.
    #[error("unknown net backend {name:?} (expected \"user\", \"loopback\", or \"vmnet\")")]
    UnknownBackend {
        /// Name from the config.
        name: String,
    },
    /// The backend exists but cannot run in this build or on this host.
    #[error("net backend {backend} unavailable: {reason}")]
    Unsupported {
        /// Backend name.
        backend: &'static str,
        /// Why it cannot run.
        reason: &'static str,
    },
    /// The vmnet helper socket failed.
    #[error("vmnet: {reason}")]
    Vmnet {
        /// Connect or frame error.
        reason: String,
    },
    /// User-mode NAT failed to bind a forward.
    #[error("user nat: {reason}")]
    User {
        /// Bind or setup error.
        reason: String,
    },
}

/// Base, size, MMIO device, and live stats from [`VirtioNet::attach`].
pub type AttachedNet = (u64, u64, Box<dyn ternvale_vmm::MmioDevice>, Arc<NetStats>);

/// Packet counters for one virtio-net device.
#[derive(Debug, Default)]
pub struct NetStats {
    /// Frames taken from the TX queue and handed to the backend.
    pub tx_packets: AtomicU64,
    /// Bytes in those frames, excluding the virtio header.
    pub tx_bytes: AtomicU64,
    /// TX chains rejected or refused by the backend.
    pub tx_dropped: AtomicU64,
    /// Frames written into the RX queue.
    pub rx_packets: AtomicU64,
    /// Bytes in those frames, excluding the virtio header.
    pub rx_bytes: AtomicU64,
    /// Backend frames that did not fit the posted RX buffer.
    pub rx_dropped: AtomicU64,
}

impl NetStats {
    /// Snapshot of `(tx_packets, tx_bytes, tx_dropped, rx_packets, rx_bytes, rx_dropped)`.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::net", skip_all)]
    pub fn snapshot(&self) -> [u64; 6] {
        [
            self.tx_packets.load(Ordering::Relaxed),
            self.tx_bytes.load(Ordering::Relaxed),
            self.tx_dropped.load(Ordering::Relaxed),
            self.rx_packets.load(Ordering::Relaxed),
            self.rx_bytes.load(Ordering::Relaxed),
            self.rx_dropped.load(Ordering::Relaxed),
        ]
    }

    pub(super) fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }
}

/// One virtio-net device with a dedicated worker thread.
pub struct VirtioNet {
    mac: [u8; 6],
    kick: Option<Sender<worker::Kick>>,
    join: Option<JoinHandle<()>>,
    stats: Arc<NetStats>,
    link: Arc<AtomicU16>,
    generation: Arc<AtomicU32>,
}

impl VirtioNet {
    /// Start the worker for `backend`. `pcap`, when set, records every frame.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::net",
        skip_all,
        fields(mac = %mac_str(&mac), backend = backend.name())
    )]
    pub fn open(
        mac: [u8; 6],
        backend: Box<dyn NetBackend>,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        pcap: Option<PcapWriter>,
        metrics: Arc<Metrics>,
    ) -> Result<Self, NetError> {
        let stats = Arc::new(NetStats::default());
        let link = Arc::new(AtomicU16::new(if backend.link_up() {
            VIRTIO_NET_S_LINK_UP
        } else {
            0
        }));
        let generation = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel();
        let mut backend = backend;
        backend.attach_policy(mac, Arc::clone(&metrics));
        let name = backend.name();
        let state = worker::Worker::new(
            backend,
            memory,
            irq,
            Arc::clone(&stats),
            worker::LinkState {
                status: Arc::clone(&link),
                generation: Arc::clone(&generation),
            },
            pcap,
            metrics,
        );
        let join = std::thread::Builder::new()
            .name("ternvale-virtio-net".into())
            .spawn(move || state.run(rx))
            .map_err(|source| {
                tracing::error!(
                    target: "ternvale::virtio::net",
                    error = %source,
                    "virtio-net worker spawn failed"
                );
                NetError::Spawn { source }
            })?;
        tracing::info!(
            target: "ternvale::virtio::net",
            mac = %mac_str(&mac),
            backend = name,
            "virtio-net device ready"
        );
        Ok(Self {
            mac,
            kick: Some(tx),
            join: Some(join),
            stats,
            link,
            generation,
        })
    }

    /// Build a transport on `slot`. Reads [`PCAP_ENV`] for an optional capture file.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::net",
        skip(backend, memory, irq_hook, metrics),
        fields(slot, mac = %mac_str(&mac))
    )]
    pub fn attach(
        slot: u32,
        mac: [u8; 6],
        backend: Box<dyn NetBackend>,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
        metrics: Arc<Metrics>,
    ) -> Result<AttachedNet, NetError> {
        let base = super::slot_base(slot).ok_or_else(|| {
            tracing::error!(target: "ternvale::virtio::net", slot, "virtio-net slot rejected");
            NetError::BadSlot { slot }
        })?;
        let pcap = PcapWriter::from_env()?;
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let net = Self::open(mac, backend, memory, Arc::clone(&irq), pcap, metrics)?;
        let stats = Arc::clone(&net.stats);
        let transport = super::mmio::VirtioMmio::with_irq(slot, Box::new(net), irq);
        Ok((
            base,
            ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE,
            Box::new(transport),
            stats,
        ))
    }

    /// Live packet counters.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::net", skip_all)]
    pub fn stats(&self) -> Arc<NetStats> {
        Arc::clone(&self.stats)
    }

    fn send(&self, kick: worker::Kick) {
        let Some(tx) = self.kick.as_ref() else {
            return;
        };
        if let Err(error) = tx.send(kick) {
            tracing::error!(
                target: "ternvale::virtio::net",
                error = %error,
                "virtio-net kick failed"
            );
        }
    }
}

impl Drop for VirtioNet {
    fn drop(&mut self) {
        self.kick.take();
        if let Some(join) = self.join.take() {
            if let Err(error) = join.join() {
                tracing::error!(
                    target: "ternvale::virtio::net",
                    ?error,
                    "virtio-net worker panicked"
                );
            }
        }
    }
}

impl VirtioDevice for VirtioNet {
    fn device_id(&self) -> u32 {
        VIRTIO_NET_ID
    }

    fn device_features(&self) -> u64 {
        VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS
    }

    fn num_queues(&self) -> u16 {
        2
    }

    fn read_config(&self, offset: u64, size: u8) -> u64 {
        let mut bytes = [0u8; 8];
        bytes[..6].copy_from_slice(&self.mac);
        let status = self.link.load(Ordering::Acquire);
        bytes[6..8].copy_from_slice(&status.to_le_bytes());
        let value = read_le(&bytes, offset, size);
        tracing::trace!(
            target: "ternvale::virtio::net",
            offset,
            size,
            value = %format!("{value:#x}"),
            "virtio-net config read"
        );
        value
    }

    fn write_config(&self, offset: u64, size: u8, value: u64) {
        tracing::warn!(
            target: "ternvale::virtio::net",
            offset,
            size,
            value = %format!("{value:#x}"),
            "ignored virtio-net config write"
        );
    }

    fn notify(&self, queue: QueueNotify) {
        self.send(worker::Kick::Notify(queue, worker::notify_mark()));
    }

    fn status_changed(&self, status: u32) {
        if status == 0 {
            self.send(worker::Kick::Reset);
        }
    }

    fn config_generation(&self) -> u32 {
        self.generation.load(Ordering::Acquire)
    }
}

/// `aa:bb:cc:dd:ee:ff`.
pub(crate) fn mac_str(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
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
mod testutil;

#[cfg(test)]
#[path = "net_tests.rs"]
mod tests;
