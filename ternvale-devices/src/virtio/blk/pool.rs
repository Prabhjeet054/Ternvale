//! Coordinator plus I/O workers: vCPU kicks never wait on host I/O.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use ternvale_metrics::Metrics;
use ternvale_vmm::GuestMemory;

use super::backend::BlkBackend;
use super::options::BlkOptions;
use super::request;
use super::{BlkStats, ID_BYTES, INT_VRING};
use crate::virtio::irq::VirtioIrq;
use crate::virtio::queue::{Chain, SplitQueue};
use crate::virtio::{QueueNotify, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};

pub(super) enum Kick {
    Notify(QueueNotify, Option<Instant>),
    Reset,
}

pub(super) struct Pool {
    pub(super) kick: Option<mpsc::Sender<Kick>>,
    coord: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
}

struct Job {
    generation: u64,
    queue: u16,
    chain: Chain,
    started: Option<Instant>,
}

struct QueueSlot {
    queue: SplitQueue,
    rings: (u16, u64, u64, u64),
}

struct Shared {
    generation: AtomicU64,
    queues: Vec<Mutex<Option<QueueSlot>>>,
    backend: BlkBackend,
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
    stats: Arc<BlkStats>,
    metrics: Arc<Metrics>,
    disk_id: Option<u8>,
    serial: [u8; ID_BYTES],
}

impl Pool {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn(
        options: BlkOptions,
        backend: BlkBackend,
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        stats: Arc<BlkStats>,
        serial: [u8; ID_BYTES],
        metrics: Arc<Metrics>,
        disk_id: Option<u8>,
    ) -> Result<Self, std::io::Error> {
        let (kick_tx, kick_rx) = mpsc::channel();
        let depth = usize::from(options.queue_depth.max(1));
        let (job_tx, job_rx) = mpsc::sync_channel(depth);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let shared = Arc::new(Shared {
            generation: AtomicU64::new(1),
            queues: (0..options.num_queues.max(1))
                .map(|_| Mutex::new(None))
                .collect(),
            backend,
            memory,
            irq,
            stats,
            metrics,
            disk_id,
            serial,
        });
        let workers = options.io_workers.max(1);
        let mut joins = Vec::with_capacity(usize::from(workers));
        for index in 0..workers {
            let rx = Arc::clone(&job_rx);
            let shared = Arc::clone(&shared);
            let join = std::thread::Builder::new()
                .name(format!("ternvale-virtio-blk-{index}"))
                .spawn(move || worker(rx, shared))?;
            joins.push(join);
        }
        let coord_shared = Arc::clone(&shared);
        let coord = std::thread::Builder::new()
            .name("ternvale-virtio-blk".into())
            .spawn(move || coordinator(kick_rx, job_tx, coord_shared))?;
        Ok(Self {
            kick: Some(kick_tx),
            coord: Some(coord),
            workers: joins,
        })
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.kick.take();
        if let Some(coord) = self.coord.take() {
            if let Err(error) = coord.join() {
                tracing::error!(
                    target: "ternvale::virtio::blk",
                    ?error,
                    "virtio-blk coordinator panicked"
                );
            }
        }
        for join in self.workers.drain(..) {
            if let Err(error) = join.join() {
                tracing::error!(
                    target: "ternvale::virtio::blk",
                    ?error,
                    "virtio-blk worker panicked"
                );
            }
        }
    }
}

fn coordinator(rx: Receiver<Kick>, jobs: SyncSender<Job>, shared: Arc<Shared>) {
    while let Ok(kick) = rx.recv() {
        match kick {
            Kick::Reset => {
                shared.generation.fetch_add(1, Ordering::Release);
                for slot in &shared.queues {
                    *ternvale_vmm::lockwatch::lock(slot, "blk-queue") = None;
                }
                tracing::debug!(target: "ternvale::virtio::blk", "virtio-blk queues reset");
            }
            Kick::Notify(notify, started) => {
                let Some(chains) = pop_chains(&shared, &notify) else {
                    continue;
                };
                let generation = shared.generation.load(Ordering::Acquire);
                let queue = notify.index;
                for chain in chains {
                    let job = Job {
                        generation,
                        queue,
                        chain,
                        started,
                    };
                    if jobs.send(job).is_err() {
                        tracing::error!(
                            target: "ternvale::virtio::blk",
                            "virtio-blk job channel closed"
                        );
                        return;
                    }
                }
            }
        }
    }
}

fn pop_chains(shared: &Shared, notify: &QueueNotify) -> Option<Vec<Chain>> {
    let Some(slot) = shared.queues.get(usize::from(notify.index)) else {
        tracing::warn!(
            target: "ternvale::virtio::blk",
            queue = notify.index,
            "blk notify for unknown queue"
        );
        return None;
    };
    let mem = ternvale_vmm::lockwatch::lock(&shared.memory, "guest-memory");
    let mut guard = ternvale_vmm::lockwatch::lock(slot, "blk-queue");
    let key = (notify.size, notify.desc, notify.avail, notify.used);
    let need_new = guard.as_ref().is_none_or(|have| have.rings != key);
    if need_new {
        let indirect = notify.features & VIRTIO_F_INDIRECT_DESC != 0;
        let event_idx = notify.features & VIRTIO_F_EVENT_IDX != 0;
        *guard = Some(QueueSlot {
            queue: SplitQueue::new(
                notify.size,
                notify.desc,
                notify.avail,
                notify.used,
                indirect,
                event_idx,
            ),
            rings: key,
        });
    }
    Some(guard.as_mut()?.queue.chains(&mem).collect())
}

fn worker(rx: Arc<Mutex<mpsc::Receiver<Job>>>, shared: Arc<Shared>) {
    loop {
        let job = {
            let guard = ternvale_vmm::lockwatch::lock(&rx, "blk-jobs");
            match guard.recv() {
                Ok(job) => job,
                Err(_) => return,
            }
        };
        if job.generation != shared.generation.load(Ordering::Acquire) {
            tracing::debug!(
                target: "ternvale::virtio::blk",
                queue = job.queue,
                "dropped blk job after reset"
            );
            continue;
        }
        run_job(&shared, job);
    }
}

fn run_job(shared: &Shared, job: Job) {
    #[cfg(not(feature = "metrics"))]
    let _ = job.started;
    let mut mem = ternvale_vmm::lockwatch::lock(&shared.memory, "guest-memory");
    #[cfg(feature = "metrics")]
    let before = shared.stats.snapshot();
    let used = request::process(
        &mut mem,
        &shared.backend,
        &job.chain,
        &shared.serial,
        &shared.stats,
    );
    #[cfg(feature = "metrics")]
    let after = shared.stats.snapshot();
    let Some(slot) = shared.queues.get(usize::from(job.queue)) else {
        return;
    };
    let mut guard = ternvale_vmm::lockwatch::lock(slot, "blk-queue");
    let Some(slot) = guard.as_mut() else {
        return;
    };
    let completed = slot.queue.add_used(&mut mem, job.chain.head, used);
    if completed && slot.queue.notification_needed(&mem) {
        shared.irq.raise(INT_VRING);
    }
    #[cfg(feature = "metrics")]
    {
        let bytes = after.1.saturating_sub(before.1);
        let elapsed = job.started.map(|at| at.elapsed());
        if let Some(id) = shared.disk_id {
            shared.metrics.record_blk_disk(id, 1, bytes, elapsed);
        } else {
            shared.metrics.record_blk(1, bytes, elapsed);
        }
    }
    #[cfg(not(feature = "metrics"))]
    let _ = &shared.metrics;
}
