//! Variable flash driven through the MMIO bus with EDK2's `VirtNorFlash.c`
//! command sequences, plus the firmware layout checks.

use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use super::pflash::BLOCK_SIZE;
use super::{check_layout, FirmwareError, RomdWindow, VarsFlash, DTB_LIMIT, MIN_RAM};
use crate::esr::ExitEvent;
use crate::mmio::{GuestRegs, MmioBus, MmioError};
use crate::platform::{FLASH_VARS_BASE, RAM_BASE};

const BANK: u64 = 4 * BLOCK_SIZE;

/// EDK2 `CREATE_DUAL_CMD`.
fn dual(cmd: u64) -> u64 {
    (cmd << 16) | cmd
}

struct Regs(RefCell<[u64; 31]>);

impl GuestRegs for Regs {
    fn get_reg(&self, index: u8) -> Result<u64, MmioError> {
        Ok(self.0.borrow()[usize::from(index)])
    }

    fn set_reg(&self, index: u8, value: u64) -> Result<(), MmioError> {
        self.0.borrow_mut()[usize::from(index)] = value;
        Ok(())
    }
}

#[derive(Clone, Default)]
struct Window {
    log: Arc<Mutex<Vec<&'static str>>>,
    fail: bool,
}

impl Window {
    fn events(&self) -> Vec<&'static str> {
        self.log.lock().expect("log").clone()
    }
}

impl RomdWindow for Window {
    fn map(&mut self, _host: NonNull<u8>, len: usize) -> Result<(), FirmwareError> {
        assert_eq!(len as u64, BANK);
        self.log.lock().expect("log").push("map");
        if self.fail {
            return Err(FirmwareError::Window {
                what: "map",
                gpa: FLASH_VARS_BASE,
                source: ternvale_hv::HvError::NoResources { code: 0 },
            });
        }
        Ok(())
    }

    fn unmap(&mut self, _len: usize) -> Result<(), FirmwareError> {
        self.log.lock().expect("log").push("unmap");
        Ok(())
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ternvale-fw-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn nvram(&self) -> PathBuf {
        self.0.join("vm").join("nvram.fd")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            panic!("remove {}: {error}", self.0.display());
        }
    }
}

struct Guest {
    bus: MmioBus,
    regs: Regs,
}

impl Guest {
    fn open(path: &std::path::Path, window: &Window) -> Self {
        let flash = VarsFlash::with_size(path, Box::new(window.clone()), BANK).expect("open flash");
        let mut bus = MmioBus::new();
        bus.register(FLASH_VARS_BASE, BANK, Box::new(flash))
            .expect("register");
        Self {
            bus,
            regs: Regs(RefCell::new([0; 31])),
        }
    }

    fn read(&self, offset: u64) -> u64 {
        let event = ExitEvent::Mmio {
            gpa: FLASH_VARS_BASE + offset,
            size: 4,
            write: false,
            reg: 1,
        };
        self.bus.dispatch(&self.regs, event).expect("read");
        self.regs.get_reg(1).expect("x1")
    }

    fn write(&self, offset: u64, value: u64) {
        self.regs.set_reg(2, value).expect("x2");
        let event = ExitEvent::Mmio {
            gpa: FLASH_VARS_BASE + offset,
            size: 4,
            write: true,
            reg: 2,
        };
        self.bus.dispatch(&self.regs, event).expect("write");
    }

    /// `NorFlashReadStatusRegister`: 0x70 at the device base, then read.
    fn status(&self) -> u64 {
        self.write(0, dual(0x70));
        self.read(0)
    }
}

const SR_WRITE: u64 = 0x0080_0080;

#[test]
fn creates_a_missing_nvram_file_and_maps_the_bank() {
    let tmp = TempDir::new("create");
    let window = Window::default();
    let guest = Guest::open(&tmp.nvram(), &window);
    assert!(tmp.nvram().is_file(), "file created with its directory");
    assert_eq!(window.events(), ["map"]);
    assert_eq!(
        guest.read(0),
        0,
        "blank store reads zero (invalid FV header)"
    );
}

#[test]
fn unlock_erase_and_status_follow_edk2() {
    let tmp = TempDir::new("erase");
    let window = Window::default();
    let guest = Guest::open(&tmp.nvram(), &window);
    let block = BLOCK_SIZE;
    // NorFlashBlockIsLocked: READ_DEVICE_ID at word 2 of the block.
    guest.write(block + 8, dual(0x90));
    assert_eq!(guest.read(block + 8) & 0x3, 0, "unlocked");
    assert_eq!(guest.read(0), 0x0089_0089, "manufacturer, doubled");
    // NorFlashUnlockSingleBlock.
    guest.write(block, dual(0x60));
    guest.write(block, dual(0xd0));
    assert_eq!(guest.status() & SR_WRITE, SR_WRITE);
    guest.write(block, dual(0xff));
    // NorFlashEraseSingleBlock.
    guest.write(block, dual(0x20));
    guest.write(block, dual(0xd0));
    let status = guest.status();
    assert_eq!(status, SR_WRITE, "ready, no error bits: {status:#x}");
    guest.write(0, dual(0xff));
    assert_eq!(guest.read(block), 0xffff_ffff);
    assert_eq!(guest.read(block + BLOCK_SIZE - 4), 0xffff_ffff);
    assert_eq!(guest.read(0), 0, "other blocks untouched");
    assert_eq!(
        window.events().last(),
        Some(&"map"),
        "{:?}",
        window.events()
    );
    assert!(window.events().contains(&"unmap"));
    drop(guest);
    let file = std::fs::read(tmp.nvram()).expect("nvram");
    assert_eq!(
        file.len() as u64,
        2 * BLOCK_SIZE,
        "written through to the end of the block"
    );
    assert!(file[BLOCK_SIZE as usize..].iter().all(|&b| b == 0xff));
}

#[test]
fn word_and_buffered_programs_persist_across_reopen() {
    let tmp = TempDir::new("program");
    let window = Window::default();
    {
        let guest = Guest::open(&tmp.nvram(), &window);
        // NorFlashWriteSingleWord.
        guest.write(0x10, dual(0x40));
        guest.write(0x10, 0x5aa5_c33c);
        assert_eq!(guest.status() & SR_WRITE, SR_WRITE);
        // NorFlashWriteBuffer: 0xE8, poll, count-1 doubled, words, confirm at base.
        guest.write(0x80, dual(0xe8));
        assert_eq!(guest.read(0x80) & SR_WRITE, SR_WRITE, "buffer available");
        let words = 32u64;
        guest.write(0x80, dual(words - 1));
        for i in 0..words {
            guest.write(0x80 + 4 * i, 0x1000_0000 + i);
        }
        guest.write(0, dual(0xd0));
        assert_eq!(guest.status(), SR_WRITE);
        guest.write(0, dual(0xff));
        assert_eq!(guest.read(0x10), 0x5aa5_c33c);
        assert_eq!(guest.read(0x80 + 4 * 31), 0x1000_001f);
    }
    let guest = Guest::open(&tmp.nvram(), &Window::default());
    assert_eq!(guest.read(0x10), 0x5aa5_c33c, "word program persisted");
    assert_eq!(guest.read(0x80), 0x1000_0000, "buffer persisted");
}

#[test]
fn bad_sequences_set_error_bits_until_clear_status() {
    let tmp = TempDir::new("bad");
    let guest = Guest::open(&tmp.nvram(), &Window::default());
    guest.write(0, dual(0x20));
    guest.write(0, dual(0xff));
    let status = guest.status();
    assert_eq!(
        status & 0x0030_0030,
        0x0030_0030,
        "sequence error: {status:#x}"
    );
    guest.write(0, dual(0x50));
    assert_eq!(guest.status(), SR_WRITE, "clear status");
    guest.write(0, dual(0xe8));
    guest.write(0, dual(0x00ff));
    assert_ne!(guest.status() & 0x0010_0010, 0, "1 KiB buffer rejected");
    guest.write(0, dual(0x50));
    guest.write(0, dual(0x33));
    assert_ne!(guest.status() & 0x0010_0010, 0, "unknown command");
}

#[test]
fn cfi_query_reports_qry() {
    let tmp = TempDir::new("cfi");
    let guest = Guest::open(&tmp.nvram(), &Window::default());
    guest.write(0x55 * 4, dual(0x98));
    let qry: Vec<u64> = (0x10..0x13).map(|i| guest.read(i * 4) & 0xff).collect();
    assert_eq!(qry, [u64::from(b'Q'), u64::from(b'R'), u64::from(b'Y')]);
    assert_eq!(guest.read(0x10 * 4), 0x0051_0051, "doubled");
}

#[test]
fn rejects_oversized_nvram_and_failed_map() {
    let tmp = TempDir::new("reject");
    let path = tmp.nvram();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    std::fs::write(&path, vec![0u8; BANK as usize + 1]).expect("big file");
    let error = VarsFlash::with_size(&path, Box::new(Window::default()), BANK)
        .err()
        .expect("too large");
    assert!(
        matches!(error, FirmwareError::NvramTooLarge { .. }),
        "{error}"
    );
    std::fs::write(&path, b"").expect("truncate");
    let failing = Window {
        fail: true,
        ..Window::default()
    };
    let error = VarsFlash::with_size(&path, Box::new(failing), BANK)
        .err()
        .expect("map fails");
    assert!(matches!(error, FirmwareError::Window { .. }), "{error}");
}

#[test]
fn layout_check_enforces_edk2_ram_and_dtb_limits() {
    check_layout(MIN_RAM, 0x2000).expect("minimum ram");
    assert!(matches!(
        check_layout(MIN_RAM - (16 << 20), 0x2000),
        Err(FirmwareError::RamTooSmall { .. })
    ));
    assert!(matches!(
        check_layout(MIN_RAM, DTB_LIMIT - RAM_BASE + 1),
        Err(FirmwareError::DtbTooLarge { .. })
    ));
}
