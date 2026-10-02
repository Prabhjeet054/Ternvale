//! virtio-blk I/O over virtio-pci: queue setup, notify, used ring, ISR, INTx.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_vmm::{GuestMemory, HOST_PAGE_SIZE};

use super::caps::{ISR_OFFSET, NOTIFY_OFFSET};
use super::tests::Guest;
use super::VirtioPci;
use crate::virtio::irq::VirtioIrq;
use crate::virtio::{
    VirtioBlk, STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK, STATUS_FEATURES_OK,
};

const RAM: u64 = 0x4000_0000;
const DESC: u64 = RAM;
const AVAIL: u64 = RAM + 0x400;
const USED: u64 = RAM + 0x800;
const HEADER: u64 = RAM + 0x1000;
const DATA: u64 = RAM + 0x1200;
const STATUS: u64 = RAM + 0x1400;
const NEXT: u16 = 1;
const WRITE: u16 = 2;

struct Disk {
    dir: std::path::PathBuf,
    image: std::path::PathBuf,
    memory: Arc<Mutex<GuestMemory>>,
}

impl Disk {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("ternvale-pci-blk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let image = dir.join("disk.img");
        std::fs::File::create(&image)
            .expect("image")
            .set_len(1 << 20)
            .expect("len");
        let mut memory = GuestMemory::new().expect("memory");
        memory.add_region(RAM, HOST_PAGE_SIZE * 2).expect("region");
        Self {
            dir,
            image,
            memory: Arc::new(Mutex::new(memory)),
        }
    }

    fn mem(&self) -> std::sync::MutexGuard<'_, GuestMemory> {
        self.memory.lock().expect("mem")
    }

    fn desc(&self, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let at = DESC + u64::from(index) * 16;
        let mut mem = self.mem();
        mem.write_u64(at, addr).expect("addr");
        mem.write_u32(at + 8, len).expect("len");
        mem.write_u16(at + 12, flags).expect("flags");
        mem.write_u16(at + 14, next).expect("next");
    }

    /// Three-descriptor request (header, data, status) published as avail entry `n`.
    fn request(&self, n: u16, kind: u32, sector: u64, data_flags: u16) {
        {
            let mut mem = self.mem();
            mem.write_u32(HEADER, kind).expect("type");
            mem.write_u32(HEADER + 4, 0).expect("reserved");
            mem.write_u64(HEADER + 8, sector).expect("sector");
            mem.write_u8(STATUS, 0xff).expect("status");
        }
        self.desc(0, HEADER, 16, NEXT, 1);
        self.desc(1, DATA, 512, NEXT | data_flags, 2);
        self.desc(2, STATUS, 1, WRITE, 0);
        let mut mem = self.mem();
        mem.write_u16(AVAIL + 4 + u64::from(n % 4) * 2, 0)
            .expect("ring");
        mem.write_u16(AVAIL + 2, n + 1).expect("idx");
    }

    fn wait_used(&self, idx: u16) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.mem().read_u16(USED + 2).expect("used idx") < idx {
            assert!(Instant::now() < deadline, "used ring did not reach {idx}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.dir) {
            tracing::warn!(error = %error, "remove test dir");
        }
    }
}

#[test]
fn blk_write_and_read_back_over_pci_with_intx() {
    let disk = Disk::new();
    let irq = VirtioIrq::new();
    let blk = VirtioBlk::open(
        &disk.image,
        false,
        Arc::clone(&disk.memory),
        Arc::clone(&irq),
    )
    .expect("blk");
    let guest =
        Guest::new(move |bdf, pin| VirtioPci::new(bdf, Box::new(blk), irq, pin, 0).expect("pci"));
    assert_eq!(
        guest.cfg(0x00, 4),
        0x1042_1af4,
        "virtio-blk is device 0x1042"
    );
    assert_eq!(guest.cfg(0x08, 4) >> 8, 0x01_00_00, "mass storage class");
    assert_eq!(
        guest.read(guest.bar0 + 0x2000, 4),
        2048,
        "capacity in sectors"
    );
    guest.negotiate();
    guest.setup_queue(4, DESC, AVAIL, USED);
    let ok = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK;
    guest.set_common(0x14, 1, u64::from(ok));
    assert_eq!(guest.common(0x14, 1), u64::from(ok));

    disk.mem().write_bytes(DATA, &[0xa5; 512]).expect("data");
    disk.request(0, 1, 3, 0);
    guest.write(guest.bar0 + NOTIFY_OFFSET, 2, 0);
    disk.wait_used(1);
    assert_eq!(
        disk.mem().read_u8(STATUS).expect("status"),
        0,
        "VIRTIO_BLK_S_OK"
    );
    let image = std::fs::read(&disk.image).expect("image");
    assert!(
        image[3 * 512..4 * 512].iter().all(|&b| b == 0xa5),
        "sector 3 written"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while !guest.line_levels().contains(&(1, true)) {
        assert!(Instant::now() < deadline, "INTx never rose");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(guest.read(guest.bar0 + ISR_OFFSET, 1), 1, "queue interrupt");
    assert_eq!(
        guest.line_levels().last(),
        Some(&(1, false)),
        "ISR read lowers INTx"
    );

    disk.mem().write_bytes(DATA, &[0; 512]).expect("clear");
    disk.request(1, 0, 3, WRITE);
    guest.write(guest.bar0 + NOTIFY_OFFSET, 2, 0);
    disk.wait_used(2);
    let mut back = [0u8; 512];
    disk.mem().read_bytes(DATA, &mut back).expect("read");
    assert!(back.iter().all(|&b| b == 0xa5), "sector 3 read back");
}
