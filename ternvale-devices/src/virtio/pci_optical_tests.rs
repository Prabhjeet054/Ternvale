//! Optical media (`[iso]` slots) over virtio-pci through the ECAM and BAR
//! windows, no VM: the features and config space a driver reads, 2048-byte
//! reads, and refused writes.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_vmm::{GuestMemory, HOST_PAGE_SIZE};

use super::caps::{DEVICE_OFFSET, NOTIFY_OFFSET};
use super::tests::Guest;
use super::VirtioPci;
use crate::virtio::irq::VirtioIrq;
use crate::virtio::{
    BlkMedia, VirtioBlk, OPTICAL_BLOCK_SIZE, STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK,
    STATUS_FEATURES_OK, VIRTIO_BLK_F_BLK_SIZE, VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_RO,
};

const RAM: u64 = 0x4000_0000;
const DESC: u64 = RAM;
const AVAIL: u64 = RAM + 0x400;
const USED: u64 = RAM + 0x800;
const HEADER: u64 = RAM + 0x1000;
const STATUS: u64 = RAM + 0x1100;
const DATA: u64 = RAM + 0x2000;
const NEXT: u16 = 1;
const WRITE: u16 = 2;
const SECTORS: usize = 3;

#[test]
fn media_selects_features_and_config_space() {
    let optical = BlkMedia::Optical;
    assert!(optical.read_only());
    assert_eq!(optical.block_size(), Some(OPTICAL_BLOCK_SIZE));
    assert_eq!(
        optical.features(),
        VIRTIO_BLK_F_RO | VIRTIO_BLK_F_BLK_SIZE,
        "no FLUSH on read-only media"
    );
    let disk = BlkMedia::Disk { read_only: false };
    assert_eq!(disk.features(), VIRTIO_BLK_F_FLUSH);
    assert_eq!(disk.block_size(), None);
    let ro = BlkMedia::Disk { read_only: true };
    assert!(ro.read_only());
    assert_eq!(ro.features(), VIRTIO_BLK_F_FLUSH | VIRTIO_BLK_F_RO);
    assert_eq!((disk.name(), optical.name()), ("disk", "optical"));
}

struct Disc {
    image: std::path::PathBuf,
    memory: Arc<Mutex<GuestMemory>>,
}

impl Disc {
    /// Three 2048-byte sectors, sector `n` filled with `n + 1`.
    fn new() -> Self {
        let image =
            std::env::temp_dir().join(format!("ternvale-optical-{}.iso", std::process::id()));
        let bytes: Vec<u8> = (1..=SECTORS as u8).flat_map(|n| vec![n; 2048]).collect();
        std::fs::write(&image, bytes).expect("image");
        let mut memory = GuestMemory::new().expect("memory");
        memory.add_region(RAM, HOST_PAGE_SIZE * 2).expect("region");
        Self {
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

    /// Submit request `n` (`kind` at 512-byte `sector`, `len` data bytes),
    /// notify, wait for it, and return its status byte.
    fn request(&self, guest: &Guest, n: u16, kind: u32, sector: u64, len: u32) -> u8 {
        {
            let mut mem = self.mem();
            mem.write_u32(HEADER, kind).expect("type");
            mem.write_u32(HEADER + 4, 0).expect("reserved");
            mem.write_u64(HEADER + 8, sector).expect("sector");
            mem.write_u8(STATUS, 0xff).expect("status");
        }
        let data_flags = if kind == 0 { NEXT | WRITE } else { NEXT };
        self.desc(0, HEADER, 16, NEXT, 1);
        self.desc(1, DATA, len, data_flags, 2);
        self.desc(2, STATUS, 1, WRITE, 0);
        {
            let mut mem = self.mem();
            mem.write_u16(AVAIL + 4 + u64::from(n % 4) * 2, 0)
                .expect("ring");
            mem.write_u16(AVAIL + 2, n + 1).expect("idx");
        }
        guest.write(guest.bar0 + NOTIFY_OFFSET, 2, 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.mem().read_u16(USED + 2).expect("used idx") < n + 1 {
            assert!(Instant::now() < deadline, "request {n} not completed");
            std::thread::sleep(Duration::from_millis(2));
        }
        self.mem().read_u8(STATUS).expect("status")
    }
}

impl Drop for Disc {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.image) {
            tracing::warn!(error = %error, "remove test image");
        }
    }
}

#[test]
fn optical_disc_reads_2048_byte_blocks_and_refuses_writes() {
    let disc = Disc::new();
    let irq = VirtioIrq::new();
    let blk = VirtioBlk::open_media(
        &disc.image,
        BlkMedia::Optical,
        Arc::clone(&disc.memory),
        Arc::clone(&irq),
    )
    .expect("optical");
    let guest =
        Guest::new(move |bdf, pin| VirtioPci::new(bdf, Box::new(blk), irq, pin, 0).expect("pci"));
    assert_eq!(
        guest.cfg(0x00, 4),
        0x1042_1af4,
        "still virtio-blk 1af4:1042"
    );
    assert_eq!(guest.cfg(0x2c, 4), 0x1100_1af4, "subsystem 1af4:1100");
    guest.set_common(0x00, 4, 0);
    let features = guest.common(0x04, 4);
    assert_eq!(
        features,
        VIRTIO_BLK_F_RO | VIRTIO_BLK_F_BLK_SIZE,
        "device features, low word"
    );
    let config = guest.bar0 + DEVICE_OFFSET;
    assert_eq!(guest.read(config, 8), 12, "capacity in 512-byte sectors");
    assert_eq!(guest.read(config + 8, 4), 0, "size_max unset");
    assert_eq!(guest.read(config + 20, 4), 2048, "blk_size");

    guest.negotiate();
    guest.setup_queue(4, DESC, AVAIL, USED);
    let ok = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK;
    guest.set_common(0x14, 1, u64::from(ok));

    // Request sectors stay 512-byte units: 2048-byte block 1 is sector 4.
    assert_eq!(disc.request(&guest, 0, 0, 4, 2048), 0, "read block 1");
    let mut block = vec![0u8; 2048];
    disc.mem().read_bytes(DATA, &mut block).expect("data");
    assert!(block.iter().all(|&b| b == 2), "block 1 holds 2s");
    disc.mem().write_bytes(DATA, &[0xa5; 512]).expect("payload");
    assert_eq!(disc.request(&guest, 1, 1, 0, 512), 1, "write is IOERR");
    assert_eq!(
        disc.request(&guest, 2, 0, 12, 512),
        1,
        "past the end is IOERR"
    );
    let image = std::fs::read(&disc.image).expect("image");
    assert!(image[..512].iter().all(|&b| b == 1), "image unchanged");
}
