//! Cache/flush mock tests, 20× integrity, and no-HV cache-mode timings.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ternvale_config::{DiskCache, DiskFlush};
use ternvale_vmm::{GuestMemory, HOST_PAGE_SIZE};

use super::backend::{BlkBackend, CountingSync, FileBackend};
use super::request;
use super::{
    BlkOptions, BlkStats, VirtioBlk, SECTOR, VIRTIO_BLK_S_OK, VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_OUT,
};
use crate::virtio::irq::VirtioIrq;
use crate::virtio::queue::{Buffer, Chain};
use crate::virtio::{QueueNotify, VirtioDevice};

const NEXT: u16 = 1;
const WRITE: u16 = 2;
const THREADS: usize = 4;
const DEPTH: usize = 8;
const SECTORS: u64 = 128;
const ROUNDS: u32 = 20;
const ID: [u8; super::ID_BYTES] = [0; super::ID_BYTES];

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn temp_image(bytes: u64) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "ternvale-blk-cache-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let image = dir.join("disk.img");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&image)
        .expect("image");
    file.set_len(bytes).expect("len");
    (dir, image)
}

fn guest_mem() -> GuestMemory {
    let mut mem = GuestMemory::new().expect("memory");
    mem.add_region(0x4000_0000, u64::from(HOST_PAGE_SIZE) * 8)
        .expect("region");
    mem
}

fn write_header(mem: &mut GuestMemory, addr: u64, ty: u32, sector: u64) {
    mem.write_u32(addr, ty).expect("type");
    mem.write_u32(addr + 4, 0).expect("reserved");
    mem.write_u64(addr + 8, sector).expect("sector");
}

fn sector_bytes(round: u32, sector: u64) -> [u8; 512] {
    let mut buf = [0u8; 512];
    buf[0..4].copy_from_slice(&round.to_le_bytes());
    buf[4..12].copy_from_slice(&sector.to_le_bytes());
    for (i, byte) in buf[12..508].iter_mut().enumerate() {
        *byte = (sector as u8)
            .wrapping_add(i as u8)
            .wrapping_add(round as u8);
    }
    let sum: u32 = buf[..508]
        .iter()
        .fold(0u32, |a, b| a.wrapping_add(u32::from(*b)));
    buf[508..512].copy_from_slice(&sum.to_le_bytes());
    buf
}

fn expected_image(round: u32, sectors: u64) -> Vec<u8> {
    let mut out = vec![0u8; (sectors * SECTOR) as usize];
    for sector in 0..sectors {
        let pat = sector_bytes(round, sector);
        let off = (sector * SECTOR) as usize;
        out[off..off + 512].copy_from_slice(&pat);
    }
    out
}

fn flush_chain(mem: &mut GuestMemory) -> Chain {
    let hdr = 0x4000_0c00;
    let status = 0x4000_1000;
    write_header(mem, hdr, VIRTIO_BLK_T_FLUSH, 0);
    mem.write_u8(status, 0xff).expect("status");
    Chain {
        head: 0,
        readable: vec![Buffer { addr: hdr, len: 16 }],
        writable: vec![Buffer {
            addr: status,
            len: 1,
        }],
    }
}

fn out_chain(mem: &mut GuestMemory, sector: u64, payload: &[u8; 512]) -> Chain {
    let hdr = 0x4000_0c00;
    let data = 0x4000_1200;
    let status = data + 512;
    write_header(mem, hdr, VIRTIO_BLK_T_OUT, sector);
    mem.write_bytes(data, payload).expect("payload");
    mem.write_u8(status, 0xff).expect("status");
    Chain {
        head: 0,
        readable: vec![
            Buffer { addr: hdr, len: 16 },
            Buffer {
                addr: data,
                len: 512,
            },
        ],
        writable: vec![Buffer {
            addr: status,
            len: 1,
        }],
    }
}

#[test]
fn guest_flush_safe_counts_full_fsync() {
    let (dir, image) = temp_image(SECTOR * 4);
    let sync = Arc::new(CountingSync::default());
    let mut options = BlkOptions::default();
    options.flush = DiskFlush::Safe;
    let backend = BlkBackend::from_file(
        FileBackend::open_with_sync(&image, false, options, Arc::clone(&sync) as _).expect("open"),
    );
    let mut mem = guest_mem();
    let chain = flush_chain(&mut mem);
    let used = request::process(&mut mem, &backend, &chain, &ID, &BlkStats::default());
    assert_eq!(used, 1);
    assert_eq!(mem.read_u8(0x4000_1000).expect("st"), VIRTIO_BLK_S_OK);
    assert_eq!(sync.full_fsync.load(Ordering::SeqCst), 1);
    assert_eq!(sync.fsync.load(Ordering::SeqCst), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn guest_flush_fast_counts_fsync_only() {
    let (dir, image) = temp_image(SECTOR * 4);
    let sync = Arc::new(CountingSync::default());
    let mut options = BlkOptions::default();
    options.flush = DiskFlush::Fast;
    let backend = BlkBackend::from_file(
        FileBackend::open_with_sync(&image, false, options, Arc::clone(&sync) as _).expect("open"),
    );
    let mut mem = guest_mem();
    let chain = flush_chain(&mut mem);
    request::process(&mut mem, &backend, &chain, &ID, &BlkStats::default());
    assert_eq!(sync.full_fsync.load(Ordering::SeqCst), 0);
    assert_eq!(sync.fsync.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn writethrough_write_counts_full_fsync() {
    let (dir, image) = temp_image(SECTOR * 4);
    let sync = Arc::new(CountingSync::default());
    let mut options = BlkOptions::default();
    options.cache = DiskCache::Writethrough;
    options.flush = DiskFlush::Safe;
    let backend = BlkBackend::from_file(
        FileBackend::open_with_sync(&image, false, options, Arc::clone(&sync) as _).expect("open"),
    );
    let mut mem = guest_mem();
    let payload = sector_bytes(1, 0);
    let chain = out_chain(&mut mem, 0, &payload);
    request::process(&mut mem, &backend, &chain, &ID, &BlkStats::default());
    assert_eq!(sync.full_fsync.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(dir);
}

struct LiveFx {
    dir: std::path::PathBuf,
    image: std::path::PathBuf,
    memory: Arc<Mutex<GuestMemory>>,
    base: u64,
    qsize: u16,
}

impl LiveFx {
    fn new(bytes: u64, options: BlkOptions) -> (Self, VirtioBlk) {
        let (dir, image) = temp_image(bytes);
        let mem = guest_mem();
        let memory = Arc::new(Mutex::new(mem));
        let irq = VirtioIrq::new();
        let blk = VirtioBlk::open_media(
            &image,
            super::BlkMedia::Disk { read_only: false },
            options,
            Arc::clone(&memory),
            irq,
            Arc::new(ternvale_metrics::Metrics::new()),
        )
        .expect("blk");
        (
            Self {
                dir,
                image,
                memory,
                base: 0x4000_0000,
                qsize: 128,
            },
            blk,
        )
    }

    fn desc(&self, index: u16) -> u64 {
        self.base + u64::from(index) * 16
    }

    fn avail(&self) -> u64 {
        self.base + 0x400
    }

    fn used(&self) -> u64 {
        self.base + 0x800
    }

    fn slot_hdr(&self, slot: u16) -> u64 {
        self.base + 0x1000 + u64::from(slot) * 16
    }

    fn slot_data(&self, slot: u16) -> u64 {
        self.base + 0x2000 + u64::from(slot) * 520
    }

    fn write_desc(&self, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let mut mem = self.memory.lock().expect("mem");
        let at = self.desc(index);
        mem.write_u64(at, addr).expect("addr");
        mem.write_u32(at + 8, len).expect("len");
        mem.write_u16(at + 12, flags).expect("flags");
        mem.write_u16(at + 14, next).expect("next");
    }

    fn notify(&self, blk: &mut VirtioBlk) {
        blk.notify(QueueNotify {
            index: 0,
            size: self.qsize,
            desc: self.base,
            avail: self.avail(),
            used: self.used(),
            features: 0,
        });
    }

    fn wait_status(&self, addr: u64) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while Instant::now() < deadline {
            let mem = self.memory.lock().expect("mem");
            if mem.read_u8(addr).expect("st") != 0xff {
                return;
            }
            drop(mem);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("status did not complete");
    }
}

impl Drop for LiveFx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn submit_out(
    fx: &LiveFx,
    blk: &Mutex<VirtioBlk>,
    avail_idx: &Mutex<u16>,
    slot: u16,
    sector: u64,
    payload: &[u8; 512],
    wait: bool,
    kick: bool,
) -> u64 {
    let hdr = fx.slot_hdr(slot);
    let data = fx.slot_data(slot);
    let status = data + 512;
    let d0 = slot * 3;
    let d1 = d0 + 1;
    let d2 = d0 + 2;
    {
        let mut mem = fx.memory.lock().expect("mem");
        write_header(&mut mem, hdr, VIRTIO_BLK_T_OUT, sector);
        mem.write_bytes(data, payload).expect("payload");
        mem.write_u8(status, 0xff).expect("status");
    }
    fx.write_desc(d0, hdr, 16, NEXT, d1);
    fx.write_desc(d1, data, 512, NEXT, d2);
    fx.write_desc(d2, status, 1, WRITE, 0);
    {
        let mut idx = avail_idx.lock().expect("avail");
        let ring = fx.avail() + 4 + u64::from(*idx % fx.qsize) * 2;
        let mut mem = fx.memory.lock().expect("mem");
        mem.write_u16(ring, d0).expect("ring");
        *idx = idx.wrapping_add(1);
        mem.write_u16(fx.avail() + 2, *idx).expect("avail idx");
    }
    if kick {
        fx.notify(&mut blk.lock().expect("blk"));
    }
    if wait {
        fx.wait_status(status);
        let st = fx.memory.lock().expect("mem").read_u8(status).expect("st");
        assert_eq!(st, VIRTIO_BLK_S_OK);
    }
    status
}

#[test]
fn integrity_twenty_power_off_rounds() {
    for round in 0..ROUNDS {
        tracing::info!(
            target: "ternvale::virtio::blk",
            round,
            threads = THREADS,
            depth = DEPTH,
            "integrity round"
        );
        let mut options = BlkOptions::default();
        options.io_workers = 4;
        options.queue_depth = 128;
        options.flush = DiskFlush::Fast;
        let (fx, blk) = LiveFx::new(SECTOR * SECTORS, options);
        let blk = Arc::new(Mutex::new(blk));
        let avail_idx = Arc::new(Mutex::new(0u16));
        let batch = Arc::new(Mutex::new(()));
        let expected = expected_image(round, SECTORS);
        std::thread::scope(|scope| {
            for tid in 0..THREADS {
                let blk = Arc::clone(&blk);
                let avail_idx = Arc::clone(&avail_idx);
                let batch = Arc::clone(&batch);
                let fx_ref = &fx;
                scope.spawn(move || {
                    let mine: Vec<u64> = (0..SECTORS)
                        .filter(|s| (*s as usize) % THREADS == tid)
                        .collect();
                    for chunk in mine.chunks(DEPTH) {
                        let _batch = batch.lock().expect("batch");
                        let mut pending = Vec::new();
                        for (i, &sector) in chunk.iter().enumerate() {
                            let slot = u16::try_from(i).expect("slot");
                            let payload = sector_bytes(round, sector);
                            pending.push(submit_out(
                                fx_ref, &blk, &avail_idx, slot, sector, &payload, false, false,
                            ));
                        }
                        fx_ref.notify(&mut blk.lock().expect("blk"));
                        for status in pending {
                            fx_ref.wait_status(status);
                            let st = fx_ref
                                .memory
                                .lock()
                                .expect("mem")
                                .read_u8(status)
                                .expect("st");
                            assert_eq!(st, VIRTIO_BLK_S_OK);
                        }
                    }
                });
            }
        });
        drop(blk);
        let got = std::fs::read(&fx.image).expect("host image");
        assert_eq!(got, expected, "round {round} host image mismatch");
        for sector in 0..SECTORS {
            let off = (sector * SECTOR) as usize;
            let sum: u32 = got[off..off + 508]
                .iter()
                .fold(0u32, |a, b| a.wrapping_add(u32::from(*b)));
            let stored = u32::from_le_bytes(got[off + 508..off + 512].try_into().expect("sum"));
            assert_eq!(sum, stored, "round {round} sector {sector} checksum");
        }
    }
}

#[test]
fn cache_mode_fio_shaped_microbench() {
    let modes = [
        DiskCache::Writeback,
        DiskCache::Writethrough,
        DiskCache::None,
    ];
    let ops = 128u32;
    let mut lines = Vec::new();
    for cache in modes {
        let mut options = BlkOptions::default();
        options.cache = cache;
        options.flush = DiskFlush::Fast;
        options.io_workers = 2;
        options.queue_depth = 64;
        let (fx, blk) = LiveFx::new(SECTOR * 512, options);
        let blk = Arc::new(Mutex::new(blk));
        let avail_idx = Mutex::new(0u16);
        let payload = [0x5au8; 512];
        let start = Instant::now();
        for i in 0..ops {
            let sector = u64::from(i % 256);
            let slot = 0;
            submit_out(&fx, &blk, &avail_idx, slot, sector, &payload, true, true);
        }
        let elapsed = start.elapsed();
        drop(blk);
        let ns = elapsed.as_nanos().max(1);
        let iops = f64::from(ops) * 1_000_000_000.0 / ns as f64;
        let kbs = iops * 512.0 / 1024.0;
        let lat_ns = ns as f64 / f64::from(ops);
        tracing::info!(
            target: "ternvale::virtio::blk",
            cache = ?cache,
            iops,
            kbs,
            lat_ns,
            "no-HV virtio-blk cache-mode stand-in (512 B OUT, not guest fio)"
        );
        lines.push(format!("{cache:?}\t{iops:.1}\t{kbs:.1}\t{lat_ns:.0}"));
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../bench-results");
    let _ = std::fs::create_dir_all(&dir);
    let body = format!("mode\tiops\tkbs\tlat_ns\n{}\n", lines.join("\n"));
    let _ = std::fs::write(dir.join("blk-cache-modes-nohv.tsv"), body);
}
