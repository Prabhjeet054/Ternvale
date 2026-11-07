//! virtio-fs (device id 26): hiprio queue plus one request queue.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ternvale_devices::{
    slot_base, Chain, IrqHook, QueueNotify, SplitQueue, VirtioDevice, VirtioIrq, VirtioMmio,
    VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC,
};
use ternvale_vmm::GuestMemory;

use crate::error::FsError;
use crate::server::{Options, Server};

/// `VIRTIO_ID_FS`.
pub const VIRTIO_FS_ID: u32 = 26;
/// Mount tag length in `virtio_fs_config`.
pub const VIRTIO_FS_TAG_LEN: usize = 36;
/// hiprio (0) + one request queue (1).
const NUM_QUEUES: u16 = 2;
const INT_VRING: u32 = 1;
const POLL: Duration = Duration::from_millis(5);

/// Counters for one virtio-fs device.
#[derive(Debug, Default)]
pub struct FsStats {
    /// FUSE requests that produced a reply (or FORGET completion).
    pub requests: AtomicU64,
    /// Response bytes written to the guest.
    pub bytes: AtomicU64,
    /// Chains that could not be served.
    pub errors: AtomicU64,
}

/// Result of [`VirtioFs::attach`].
pub type AttachedFs = (u64, u64, Box<dyn ternvale_vmm::MmioDevice>, Arc<FsStats>);

enum Kick {
    Notify(QueueNotify),
    Reset,
}

struct Bound {
    key: (u16, u64, u64, u64),
    queue: SplitQueue,
}

/// virtio-fs device exporting `root` under mount tag `tag`.
pub struct VirtioFs {
    tag: [u8; VIRTIO_FS_TAG_LEN],
    kick: Option<mpsc::Sender<Kick>>,
    join: Option<JoinHandle<()>>,
    stats: Arc<FsStats>,
}

impl VirtioFs {
    /// Build the device and start the worker thread.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtiofs",
        skip(memory, irq),
        fields(tag = %tag, root = %root.display(), read_only = opts.read_only)
    )]
    pub fn open(
        root: PathBuf,
        tag: &str,
        opts: Options,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
    ) -> Result<Self, FsError> {
        let mut tag_bytes = [0u8; VIRTIO_FS_TAG_LEN];
        let bytes = tag.as_bytes();
        let copy = bytes.len().min(VIRTIO_FS_TAG_LEN);
        tag_bytes[..copy].copy_from_slice(&bytes[..copy]);
        let stats = Arc::new(FsStats::default());
        let (tx, rx) = mpsc::channel();
        let worker_memory = Arc::clone(&memory);
        let worker_irq = Arc::clone(&irq);
        let worker_stats = Arc::clone(&stats);
        let export = root.clone();
        let join = std::thread::Builder::new()
            .name("virtio-fs".into())
            .spawn(move || worker_loop(export, opts, rx, worker_memory, worker_irq, worker_stats))
            .map_err(FsError::Worker)?;
        tracing::info!(
            target: "ternvale::virtiofs",
            tag,
            root = %root.display(),
            read_only = opts.read_only,
            "virtio-fs device ready"
        );
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
        target = "ternvale::virtiofs",
        skip(memory, irq_hook),
        fields(slot, tag)
    )]
    pub fn attach(
        slot: u32,
        root: PathBuf,
        tag: &str,
        opts: Options,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
    ) -> Result<AttachedFs, FsError> {
        let base = slot_base(slot).ok_or(FsError::BadSlot { slot })?;
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let fs = Self::open(root, tag, opts, memory, Arc::clone(&irq))?;
        let stats = Arc::clone(&fs.stats);
        let transport = VirtioMmio::with_irq(slot, Box::new(fs), irq);
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
                tracing::error!(target: "ternvale::virtiofs", error = %error, "virtio-fs kick failed");
            }
        }
    }
}

impl Drop for VirtioFs {
    fn drop(&mut self) {
        self.kick.take();
        if let Some(join) = self.join.take() {
            if join.join().is_err() {
                tracing::error!(target: "ternvale::virtiofs", "virtio-fs worker panicked");
            }
        }
    }
}

fn worker_loop(
    root: PathBuf,
    opts: Options,
    kicks: mpsc::Receiver<Kick>,
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
    stats: Arc<FsStats>,
) {
    let mut server = match Server::open(&root, opts) {
        Ok(server) => server,
        Err(error) => {
            tracing::error!(target: "ternvale::virtiofs", error = %error, "virtio-fs export open failed");
            return;
        }
    };
    let mut bound: HashMap<u16, Bound> = HashMap::new();
    tracing::debug!(target: "ternvale::virtiofs", "virtio-fs worker started");
    loop {
        match kicks.recv_timeout(POLL) {
            Ok(Kick::Notify(notify)) => bind_queue(&mut bound, &notify),
            Ok(Kick::Reset) => {
                bound.clear();
                match Server::open(&root, opts) {
                    Ok(next) => server = next,
                    Err(error) => tracing::error!(
                        target: "ternvale::virtiofs",
                        error = %error,
                        "virtio-fs reset reopen failed"
                    ),
                }
                continue;
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
        for queue in bound.values_mut() {
            serve_queue(&mut server, queue, &memory, &irq, &stats);
        }
    }
    tracing::debug!(target: "ternvale::virtiofs", "virtio-fs worker stopped");
}

fn bind_queue(bound: &mut HashMap<u16, Bound>, notify: &QueueNotify) {
    if notify.index >= NUM_QUEUES {
        tracing::warn!(
            target: "ternvale::virtiofs",
            queue = notify.index,
            reason = "notify for unknown virtio-fs queue",
            "rejected share queue"
        );
        return;
    }
    let key = (notify.size, notify.desc, notify.avail, notify.used);
    if bound.get(&notify.index).is_some_and(|b| b.key == key) {
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
    bound.insert(notify.index, Bound { key, queue });
    tracing::debug!(
        target: "ternvale::virtiofs",
        queue = notify.index,
        size = notify.size,
        "virtio-fs queue bound"
    );
}

fn serve_queue(
    server: &mut Server,
    bound: &mut Bound,
    memory: &Arc<Mutex<GuestMemory>>,
    irq: &Arc<VirtioIrq>,
    stats: &Arc<FsStats>,
) {
    let mut completed = false;
    let mut mem = ternvale_vmm::lockwatch::lock(memory, "guest-memory");
    let chains: Vec<Chain> = bound.queue.chains(&mem).collect();
    for chain in chains {
        let request = read_chain(&mem, &chain);
        let response = match request {
            Some(req) => server.handle(&req).unwrap_or_default(),
            None => {
                stats.errors.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            }
        };
        let used = write_chain(&mut mem, &chain, &response);
        if used > 0 || request_was_forget(&response) {
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

fn request_was_forget(response: &[u8]) -> bool {
    response.is_empty()
}

fn read_chain(mem: &GuestMemory, chain: &Chain) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for buf in &chain.readable {
        let mut chunk = vec![0u8; buf.len as usize];
        if mem.read_bytes(buf.addr, &mut chunk).is_err() {
            tracing::warn!(
                target: "ternvale::virtiofs",
                addr = %format!("{:#x}", buf.addr),
                reason = "request descriptor read failed",
                "rejected share chain"
            );
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
            tracing::warn!(
                target: "ternvale::virtiofs",
                addr = %format!("{:#x}", buf.addr),
                reason = "response descriptor write failed",
                "rejected share chain"
            );
            return 0;
        }
        offset += take;
    }
    offset as u32
}

impl VirtioDevice for VirtioFs {
    fn device_id(&self) -> u32 {
        VIRTIO_FS_ID
    }

    fn num_queues(&self) -> u16 {
        NUM_QUEUES
    }

    fn read_config(&self, offset: u64, size: u8) -> u64 {
        // virtio 1.2 §5.11.4: tag[36] then le32 num_request_queues.
        let mut cfg = [0u8; VIRTIO_FS_TAG_LEN + 4];
        cfg[..VIRTIO_FS_TAG_LEN].copy_from_slice(&self.tag);
        cfg[VIRTIO_FS_TAG_LEN..VIRTIO_FS_TAG_LEN + 4].copy_from_slice(&1u32.to_le_bytes());
        let mut tmp = [0u8; 8];
        let start = usize::try_from(offset).unwrap_or(cfg.len());
        if start < cfg.len() {
            let n = usize::from(size).min(cfg.len() - start).min(8);
            tmp[..n].copy_from_slice(&cfg[start..start + n]);
        }
        u64::from_le_bytes(tmp)
    }

    fn notify(&self, queue: QueueNotify) {
        self.send(Kick::Notify(queue));
    }

    fn status_changed(&self, status: u32) {
        if status == 0 {
            self.send(Kick::Reset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{VirtioFs, VIRTIO_FS_ID, VIRTIO_FS_TAG_LEN};
    use ternvale_devices::VirtioDevice;

    #[test]
    fn config_reports_tag_and_one_request_queue() {
        // Device identity without attaching MMIO: pack tag like `open` does.
        let mut tag = [0u8; VIRTIO_FS_TAG_LEN];
        tag[..4].copy_from_slice(b"tern");
        let fs = VirtioFs {
            tag,
            kick: None,
            join: None,
            stats: Default::default(),
        };
        assert_eq!(fs.device_id(), VIRTIO_FS_ID);
        assert_eq!(fs.num_queues(), 2);
        let word = fs.read_config(VIRTIO_FS_TAG_LEN as u64, 4);
        assert_eq!(word as u32, 1);
    }
}
