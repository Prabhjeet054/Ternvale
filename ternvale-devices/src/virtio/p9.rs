//! virtio-9p (device id 9): export a host directory to the guest over 9P2000.L.
//!
//! virtio-fs would need a FUSE daemon on the host; this device implements a
//! minimal in-process 9p2000.L server instead (see `docs/SHARE.md`).

mod server;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ternvale_vmm::GuestMemory;

use super::irq::{IrqHook, VirtioIrq};
use super::queue::{Chain, SplitQueue};
use super::{QueueNotify, VirtioDevice, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};

use server::Server;

/// Virtio 9p device id.
pub const VIRTIO_9P_ID: u32 = 9;
/// Mount tag length in virtio config (bytes).
pub const VIRTIO_9P_TAG_LEN: usize = 36;

const INT_VRING: u32 = 1;
const POLL: Duration = Duration::from_millis(5);

/// virtio-9p could not be attached.
#[derive(Debug, thiserror::Error)]
pub enum P9Error {
    /// MMIO slot out of range.
    #[error("virtio-9p slot {slot} is outside the virtio-mmio window")]
    BadSlot {
        /// Requested slot.
        slot: u32,
    },
    /// Host export path is invalid.
    #[error("virtio-9p export {path}: {source}")]
    Export {
        /// Host path.
        path: PathBuf,
        /// OS error.
        source: std::io::Error,
    },
}

/// Counters for one virtio-9p device.
#[derive(Debug, Default)]
pub struct P9Stats {
    /// 9p requests served.
    pub requests: AtomicU64,
    /// Response bytes written to the guest.
    pub bytes: AtomicU64,
    /// Chains completed with an error or empty response.
    pub errors: AtomicU64,
}

/// Result of [`VirtioP9::attach`].
pub type AttachedP9 = (u64, u64, Box<dyn ternvale_vmm::MmioDevice>, Arc<P9Stats>);

enum Kick {
    Notify(QueueNotify),
    Reset,
}

struct Bound {
    key: (u16, u64, u64, u64),
    queue: SplitQueue,
}

/// virtio-9p device exporting `root` under virtio mount tag `tag`.
pub struct VirtioP9 {
    tag: [u8; VIRTIO_9P_TAG_LEN],
    kick: Option<mpsc::Sender<Kick>>,
    join: Option<JoinHandle<()>>,
    stats: Arc<P9Stats>,
}

impl VirtioP9 {
    /// Build the device and start the worker thread.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::p9",
        skip(memory, irq),
        fields(tag = %tag, root = %root.display())
    )]
    pub fn open(
        root: PathBuf,
        tag: &str,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
    ) -> Result<Self, P9Error> {
        if !root.is_dir() {
            return Err(P9Error::Export {
                path: root,
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            });
        }
        let mut tag_bytes = [0u8; VIRTIO_9P_TAG_LEN];
        let bytes = tag.as_bytes();
        let copy = bytes.len().min(VIRTIO_9P_TAG_LEN);
        tag_bytes[..copy].copy_from_slice(&bytes[..copy]);
        let stats = Arc::new(P9Stats::default());
        let (tx, rx) = mpsc::channel();
        let worker_memory = Arc::clone(&memory);
        let worker_irq = Arc::clone(&irq);
        let worker_stats = Arc::clone(&stats);
        let export_path = root.clone();
        let log_root = export_path.clone();
        let join = std::thread::Builder::new()
            .name("virtio-p9".into())
            .spawn(move || worker_loop(root, rx, worker_memory, worker_irq, worker_stats))
            .map_err(|source| P9Error::Export {
                path: export_path,
                source,
            })?;
        tracing::info!(target: "ternvale::virtio::p9", tag, root = %log_root.display(), "virtio-9p device ready");
        Ok(Self {
            tag: tag_bytes,
            kick: Some(tx),
            join: Some(join),
            stats,
        })
    }

    /// Attach on virtio-mmio `slot`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::p9",
        skip(memory, irq_hook),
        fields(slot, tag)
    )]
    pub fn attach(
        slot: u32,
        root: PathBuf,
        tag: &str,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
    ) -> Result<AttachedP9, P9Error> {
        let base = super::slot_base(slot).ok_or(P9Error::BadSlot { slot })?;
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let p9 = Self::open(root, tag, memory, Arc::clone(&irq))?;
        let stats = Arc::clone(&p9.stats);
        let transport = super::mmio::VirtioMmio::with_irq(slot, Box::new(p9), irq);
        Ok((
            base,
            ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE,
            Box::new(transport),
            stats,
        ))
    }

    fn send(&self, kick: Kick) {
        if let Some(tx) = self.kick.as_ref() {
            if let Err(error) = tx.send(kick) {
                tracing::error!(target: "ternvale::virtio::p9", error = %error, "virtio-9p kick failed");
            }
        }
    }
}

impl Drop for VirtioP9 {
    fn drop(&mut self) {
        self.kick.take();
        if let Some(join) = self.join.take() {
            if join.join().is_err() {
                tracing::error!(target: "ternvale::virtio::p9", "virtio-9p worker panicked");
            }
        }
    }
}

fn worker_loop(
    root: PathBuf,
    kicks: mpsc::Receiver<Kick>,
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
    stats: Arc<P9Stats>,
) {
    let export = root.clone();
    let mut server = Server::new(root);
    let mut bound: Option<Bound> = None;
    tracing::debug!(target: "ternvale::virtio::p9", "virtio-9p worker started");
    loop {
        match kicks.recv_timeout(POLL) {
            Ok(Kick::Notify(notify)) => bind_queue(&mut bound, &notify),
            Ok(Kick::Reset) => {
                bound = None;
                server = Server::new(export.clone());
                continue;
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Some(bound) = bound.as_mut() {
            serve_queue(&mut server, bound, &memory, &irq, &stats);
        }
    }
    tracing::debug!(target: "ternvale::virtio::p9", "virtio-9p worker stopped");
}

fn bind_queue(bound: &mut Option<Bound>, notify: &QueueNotify) {
    if notify.index != 0 {
        tracing::warn!(target: "ternvale::virtio::p9", queue = notify.index, "notify for unknown queue");
        return;
    }
    let key = (notify.size, notify.desc, notify.avail, notify.used);
    if bound.as_ref().is_some_and(|b| b.key == key) {
        return;
    }
    let queue = SplitQueue::new(
        notify.size,
        notify.desc,
        notify.avail,
        notify.used,
        notify.features & VIRTIO_F_INDIRECT_DESC != 0,
        notify.features & VIRTIO_F_EVENT_IDX != 0,
    );
    *bound = Some(Bound { key, queue });
    tracing::debug!(target: "ternvale::virtio::p9", size = notify.size, "virtio-9p queue bound");
}

fn serve_queue(
    server: &mut Server,
    bound: &mut Bound,
    memory: &Arc<Mutex<GuestMemory>>,
    irq: &Arc<VirtioIrq>,
    stats: &Arc<P9Stats>,
) {
    let mut completed = false;
    let mut mem = ternvale_vmm::lockwatch::lock(memory, "guest-memory");
    let chains: Vec<Chain> = bound.queue.chains(&mem).collect();
    for chain in chains {
        let request = read_chain(&mem, &chain);
        let response = match request {
            Some(req) => server.handle(&req),
            None => {
                stats.errors.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            }
        };
        let used = write_chain(&mut mem, &chain, &response);
        if used > 0 {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            stats.bytes.fetch_add(u64::from(used), Ordering::Relaxed);
        } else if !response.is_empty() {
            stats.errors.fetch_add(1, Ordering::Relaxed);
        }
        completed |= bound.queue.add_used(&mut mem, chain.head, used);
    }
    if completed && bound.queue.notification_needed(&mem) {
        irq.raise(INT_VRING);
    }
}

fn read_chain(mem: &GuestMemory, chain: &Chain) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for buf in &chain.readable {
        let mut chunk = vec![0u8; buf.len as usize];
        if mem.read_bytes(buf.addr, &mut chunk).is_err() {
            tracing::warn!(target: "ternvale::virtio::p9", addr = %format!("{:#x}", buf.addr), "9p request read failed");
            return None;
        }
        out.extend_from_slice(&chunk);
    }
    Some(out)
}

fn write_chain(mem: &mut GuestMemory, chain: &Chain, response: &[u8]) -> u32 {
    if response.is_empty() {
        return 0;
    }
    let mut offset = 0usize;
    for buf in &chain.writable {
        let take = (buf.len as usize).min(response.len() - offset);
        if take == 0 {
            break;
        }
        if mem
            .write_bytes(buf.addr, &response[offset..offset + take])
            .is_err()
        {
            tracing::warn!(target: "ternvale::virtio::p9", addr = %format!("{:#x}", buf.addr), "9p response write failed");
            return 0;
        }
        offset += take;
    }
    offset as u32
}

impl VirtioDevice for VirtioP9 {
    fn device_id(&self) -> u32 {
        VIRTIO_9P_ID
    }

    fn num_queues(&self) -> u16 {
        1
    }

    fn read_config(&mut self, offset: u64, size: u8) -> u64 {
        let mut tmp = [0u8; 8];
        let start = usize::try_from(offset).unwrap_or(VIRTIO_9P_TAG_LEN + 2);
        if start < VIRTIO_9P_TAG_LEN {
            let n = usize::from(size).min(VIRTIO_9P_TAG_LEN - start);
            tmp[..n].copy_from_slice(&self.tag[start..start + n]);
        } else if start == VIRTIO_9P_TAG_LEN && size >= 2 {
            tmp[0] = 1;
            tmp[1] = 0;
        }
        u64::from_le_bytes(tmp)
    }

    fn notify(&mut self, queue: QueueNotify) {
        self.send(Kick::Notify(queue));
    }

    fn status_changed(&mut self, status: u32) {
        if status == 0 {
            self.send(Kick::Reset);
        }
    }
}
