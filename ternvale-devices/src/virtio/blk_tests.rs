//! Unit tests for virtio-blk against a temp image and fake guest memory.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_vmm::{GuestMemory, HOST_PAGE_SIZE};

use super::backend::FileBackend;
use super::{
    VirtioBlk, SECTOR, VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_RO, VIRTIO_BLK_S_IOERR, VIRTIO_BLK_S_OK,
    VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
};
use crate::virtio::irq::VirtioIrq;
use crate::virtio::{QueueNotify, VirtioDevice};

const NEXT: u16 = 1;
const WRITE: u16 = 2;

struct Fixture {
    dir: std::path::PathBuf,
    memory: Arc<Mutex<GuestMemory>>,
    raised: Arc<AtomicBool>,
    base: u64,
}

impl Fixture {
    fn new(bytes: u64, read_only: bool) -> (Self, VirtioBlk) {
        let dir =
            std::env::temp_dir().join(format!("ternvale-blk-{}-{}", std::process::id(), bytes));
        std::fs::create_dir_all(&dir).expect("dir");
        let image = dir.join("disk.img");
        {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&image)
                .expect("image");
            file.set_len(bytes).expect("len");
        }
        let mut mem = GuestMemory::new().expect("memory");
        let base = 0x4000_0000u64;
        mem.add_region(base, u64::from(HOST_PAGE_SIZE) * 2)
            .expect("region");
        let memory = Arc::new(Mutex::new(mem));
        let irq = VirtioIrq::new();
        let raised = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&raised);
        irq.set_hook(Arc::new(move |level| {
            flag.store(level, Ordering::Release);
        }));
        let blk =
            VirtioBlk::open(&image, read_only, Arc::clone(&memory), Arc::clone(&irq)).expect("blk");
        (
            Self {
                dir,
                memory,
                raised,
                base,
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

    fn data(&self) -> u64 {
        self.base + 0x1000
    }

    fn write_desc(&self, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let mut mem = self.memory.lock().expect("mem");
        let at = self.desc(index);
        mem.write_u64(at, addr).expect("addr");
        mem.write_u32(at + 8, len).expect("len");
        mem.write_u16(at + 12, flags).expect("flags");
        mem.write_u16(at + 14, next).expect("next");
    }

    fn publish(&self, head: u16) {
        let mut mem = self.memory.lock().expect("mem");
        mem.write_u16(self.avail() + 4, head).expect("ring");
        mem.write_u16(self.avail() + 2, 1).expect("idx");
    }

    fn notify(&self, blk: &mut VirtioBlk) {
        blk.notify(QueueNotify {
            index: 0,
            size: 4,
            desc: self.base,
            avail: self.avail(),
            used: self.used(),
            features: 0,
        });
    }

    fn wait_used(&self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let mem = self.memory.lock().expect("mem");
            if mem.read_u16(self.used() + 2).expect("idx") >= 1 {
                return;
            }
            drop(mem);
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("used ring did not advance");
    }

    fn status(&self) -> u8 {
        let mem = self.memory.lock().expect("mem");
        mem.read_u8(self.data() + 512).expect("status")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write_header(mem: &mut GuestMemory, addr: u64, ty: u32, sector: u64) {
    mem.write_u32(addr, ty).expect("type");
    mem.write_u32(addr + 4, 0).expect("reserved");
    mem.write_u64(addr + 8, sector).expect("sector");
}

#[test]
fn config_reports_capacity_and_flush() {
    let (fx, mut blk) = Fixture::new(SECTOR * 8, false);
    assert_eq!(blk.read_config(0, 8), 8);
    assert_eq!(
        blk.device_features() & VIRTIO_BLK_F_FLUSH,
        VIRTIO_BLK_F_FLUSH
    );
    assert_eq!(blk.device_features() & VIRTIO_BLK_F_RO, 0);
    drop(fx);
}

#[test]
fn read_write_round_trip() {
    let (fx, mut blk) = Fixture::new(SECTOR * 8, false);
    let hdr = fx.base + 0xc00;
    let data = fx.data();
    {
        let mut mem = fx.memory.lock().expect("mem");
        write_header(&mut mem, hdr, VIRTIO_BLK_T_OUT, 1);
        mem.write_bytes(data, &[0xab; 512]).expect("payload");
        mem.write_u8(data + 512, 0xff).expect("status");
    }
    fx.write_desc(0, hdr, 16, NEXT, 1);
    fx.write_desc(1, data, 512, NEXT, 2);
    fx.write_desc(2, data + 512, 1, WRITE, 0);
    fx.publish(0);
    fx.notify(&mut blk);
    fx.wait_used();
    assert_eq!(fx.status(), VIRTIO_BLK_S_OK);
    assert!(fx.raised.load(Ordering::Acquire));
    assert_eq!(blk.stats().snapshot().0, 1);

    // Reset used idx for a second request by rewriting avail.
    {
        let mut mem = fx.memory.lock().expect("mem");
        mem.write_u16(fx.used() + 2, 0).expect("clear used");
        write_header(&mut mem, hdr, VIRTIO_BLK_T_IN, 1);
        mem.write_bytes(data, &[0; 512]).expect("clear");
        mem.write_u8(data + 512, 0xff).expect("status");
    }
    // Rebuild queue state via reset then new notify with fresh avail idx.
    blk.status_changed(0);
    std::thread::sleep(Duration::from_millis(20));
    {
        let mut mem = fx.memory.lock().expect("mem");
        mem.write_u16(fx.avail() + 2, 0).expect("reset avail");
        mem.write_u16(fx.avail() + 2, 1).expect("publish");
    }
    fx.write_desc(0, hdr, 16, NEXT, 1);
    fx.write_desc(1, data, 512, WRITE | NEXT, 2);
    fx.write_desc(2, data + 512, 1, WRITE, 0);
    fx.notify(&mut blk);
    fx.wait_used();
    assert_eq!(fx.status(), VIRTIO_BLK_S_OK);
    let mem = fx.memory.lock().expect("mem");
    let mut buf = [0u8; 512];
    mem.read_bytes(data, &mut buf).expect("read back");
    assert!(buf.iter().all(|&b| b == 0xab));
}

#[test]
fn out_of_range_sector_returns_ioerr() {
    let (fx, mut blk) = Fixture::new(SECTOR * 4, false);
    let hdr = fx.base + 0xc00;
    let data = fx.data();
    {
        let mut mem = fx.memory.lock().expect("mem");
        write_header(&mut mem, hdr, VIRTIO_BLK_T_IN, 100);
        mem.write_u8(data + 512, 0xff).expect("status");
    }
    fx.write_desc(0, hdr, 16, NEXT, 1);
    fx.write_desc(1, data, 512, WRITE | NEXT, 2);
    fx.write_desc(2, data + 512, 1, WRITE, 0);
    fx.publish(0);
    fx.notify(&mut blk);
    fx.wait_used();
    assert_eq!(fx.status(), VIRTIO_BLK_S_IOERR);
    assert_eq!(blk.stats().snapshot().2, 1);
}

#[test]
fn read_only_rejects_writes() {
    let (fx, mut blk) = Fixture::new(SECTOR * 4, true);
    assert_ne!(blk.device_features() & VIRTIO_BLK_F_RO, 0);
    let hdr = fx.base + 0xc00;
    let data = fx.data();
    {
        let mut mem = fx.memory.lock().expect("mem");
        write_header(&mut mem, hdr, VIRTIO_BLK_T_OUT, 0);
        mem.write_bytes(data, &[1; 512]).expect("payload");
        mem.write_u8(data + 512, 0xff).expect("status");
    }
    fx.write_desc(0, hdr, 16, NEXT, 1);
    fx.write_desc(1, data, 512, NEXT, 2);
    fx.write_desc(2, data + 512, 1, WRITE, 0);
    fx.publish(0);
    fx.notify(&mut blk);
    fx.wait_used();
    assert_eq!(fx.status(), VIRTIO_BLK_S_IOERR);
}

#[test]
fn flush_succeeds() {
    let (fx, mut blk) = Fixture::new(SECTOR * 4, false);
    let hdr = fx.base + 0xc00;
    let status = fx.data();
    {
        let mut mem = fx.memory.lock().expect("mem");
        write_header(&mut mem, hdr, VIRTIO_BLK_T_FLUSH, 0);
        mem.write_u8(status, 0xff).expect("status");
    }
    fx.write_desc(0, hdr, 16, NEXT, 1);
    fx.write_desc(1, status, 1, WRITE, 0);
    fx.publish(0);
    fx.notify(&mut blk);
    fx.wait_used();
    let mem = fx.memory.lock().expect("mem");
    assert_eq!(mem.read_u8(status).expect("st"), VIRTIO_BLK_S_OK);
}

#[test]
fn backend_pread_pwrite_round_trip() {
    let dir = std::env::temp_dir().join(format!("ternvale-blk-be-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("img");
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .expect("create")
        .set_len(SECTOR * 2)
        .expect("len");
    let be = FileBackend::open(&path, false).expect("open");
    be.write_at(&[0xcd; 512], SECTOR).expect("write");
    let mut buf = [0u8; 512];
    be.read_at(&mut buf, SECTOR).expect("read");
    assert!(buf.iter().all(|&b| b == 0xcd));
    be.flush().expect("flush");
    let _ = std::fs::remove_dir_all(dir);
}
