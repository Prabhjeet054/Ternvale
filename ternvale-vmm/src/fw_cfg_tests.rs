use std::cell::RefCell;

use super::{FwCfg, FW_CFG_REG_SIZE};
use crate::esr::ExitEvent;
use crate::firmware::FirmwareError;
use crate::mmio::{GuestRegs, MmioBus, MmioError};
use crate::platform::{ACPI_BASE, ACPI_SIZE, FW_CFG_BASE};

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

/// A guest driving fw_cfg the way EDK2's `QemuFwCfgLibMmio.c` does on AArch64.
struct Edk2 {
    bus: MmioBus,
    regs: Regs,
}

impl Edk2 {
    fn new(files: Vec<(String, Vec<u8>)>) -> Self {
        let mut bus = MmioBus::new();
        let device = FwCfg::new(files).expect("fw_cfg");
        bus.register(FW_CFG_BASE, FW_CFG_REG_SIZE, Box::new(device))
            .expect("register");
        Self {
            bus,
            regs: Regs(RefCell::new([0; 31])),
        }
    }

    fn load(&self, offset: u64, size: u8) -> u64 {
        let event = ExitEvent::Mmio {
            gpa: FW_CFG_BASE + offset,
            size,
            write: false,
            reg: 3,
        };
        self.bus.dispatch(&self.regs, event).expect("load");
        self.regs.get_reg(3).expect("x3")
    }

    fn store(&self, offset: u64, size: u8, value: u64) {
        self.regs.set_reg(4, value).expect("x4");
        let event = ExitEvent::Mmio {
            gpa: FW_CFG_BASE + offset,
            size,
            write: true,
            reg: 4,
        };
        self.bus.dispatch(&self.regs, event).expect("store");
    }

    /// `QemuFwCfgSelectItem`: `MmioWrite16 (Selector, SwapBytes16 (Item))`.
    fn select(&self, item: u16) {
        self.store(8, 2, u64::from(item.swap_bytes()));
    }

    /// `MmioReadBytes` on AArch64: 8-byte loads, then 4, 2, 1 for the tail.
    fn read_bytes(&self, size: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(size);
        let left = size & 7;
        for _ in 0..size / 8 {
            out.extend_from_slice(&self.load(0, 8).to_le_bytes());
        }
        for (bit, width) in [(4, 4u8), (2, 2), (1, 1)] {
            if left & bit != 0 {
                let value = self.load(0, width).to_le_bytes();
                out.extend_from_slice(&value[..usize::from(width)]);
            }
        }
        out
    }

    fn read32(&self) -> u32 {
        u32::from_le_bytes(self.read_bytes(4).try_into().unwrap())
    }

    /// `QemuFwCfgFindFile`.
    fn find_file(&self, name: &str) -> Option<(u16, usize)> {
        self.select(0x19);
        let count = self.read32().swap_bytes();
        for _ in 0..count {
            let size = self.read32().swap_bytes();
            let select = u16::from_le_bytes(self.read_bytes(2).try_into().unwrap()).swap_bytes();
            self.read_bytes(2);
            let field = self.read_bytes(56);
            let end = field.iter().position(|&b| b == 0).unwrap_or(56);
            if &field[..end] == name.as_bytes() {
                return Some((select, size as usize));
            }
        }
        None
    }
}

#[test]
fn signature_features_and_files_read_like_qemu() {
    let edk2 = Edk2::new(vec![
        ("etc/one".into(), (0u8..=20).collect()),
        ("opt/two".into(), b"hello".to_vec()),
    ]);
    edk2.select(0);
    assert_eq!(edk2.read32(), u32::from_le_bytes(*b"QEMU"));
    edk2.select(1);
    assert_eq!(
        edk2.read32() & 2,
        0,
        "no DMA, so EDK2 keeps the data register"
    );
    let (select, size) = edk2.find_file("etc/one").expect("etc/one");
    assert_eq!((select, size), (0x20, 21));
    edk2.select(select);
    assert_eq!(edk2.read_bytes(size), (0u8..=20).collect::<Vec<_>>());
    assert_eq!(edk2.read_bytes(3), [0, 0, 0], "past the end reads 0");
    edk2.select(select);
    assert_eq!(edk2.load(0, 2), 0x0100, "a select rewinds");
    assert_eq!(edk2.find_file("opt/two"), Some((0x21, 5)));
    assert_eq!(edk2.find_file("etc/table-loader"), None);
    edk2.select(0x0008);
    assert_eq!(edk2.read32(), 0, "kernel size: not provided");
}

#[test]
fn unsupported_accesses_are_refused_without_side_effects() {
    let edk2 = Edk2::new(vec![("etc/one".into(), b"abcdefgh".to_vec())]);
    edk2.select(0x20);
    assert_eq!(edk2.load(8, 2), 0, "the selector reads 0");
    edk2.store(0x10, 8, 0x1234);
    edk2.store(0, 1, 0xff);
    edk2.store(8, 4, 0x2100_0000);
    assert_eq!(edk2.load(0, 8), u64::from_le_bytes(*b"abcdefgh"));
}

#[test]
fn bad_file_sets_are_rejected() {
    let long = "x".repeat(56);
    for (files, needle) in [
        (vec![(String::new(), vec![])], "1..56"),
        (vec![(long, vec![])], "1..56"),
        (vec![("a\0b".into(), vec![])], "NUL"),
        (
            vec![("a".into(), vec![]), ("a".into(), vec![])],
            "duplicate",
        ),
    ] {
        let error = FwCfg::new(files).unwrap_err();
        assert!(matches!(error, FirmwareError::FwCfgFile { .. }), "{error}");
        assert!(error.to_string().contains(needle), "{error}");
    }
}

#[test]
fn edk2_reads_the_acpi_loader_files_back_intact() {
    let tables =
        ternvale_acpi::AcpiTables::build(ACPI_BASE, ACPI_SIZE, &crate::acpi_check::acpi_config(4))
            .expect("tables");
    let refs: Vec<_> = tables
        .tables()
        .iter()
        .map(ternvale_acpi::TableRef::from)
        .collect();
    let blobs = ternvale_acpi::LoaderBlobs::build(&refs).expect("blobs");
    let edk2 = Edk2::new(blobs.files().expect("files"));
    let (item, size) = edk2
        .find_file(ternvale_acpi::LOADER_FILE)
        .expect("PlatformHasAcpiDtDxe picks ACPI");
    edk2.select(item);
    let script = edk2.read_bytes(size);
    assert_eq!(script, blobs.loader().expect("loader"));
    let commands: Vec<_> = script
        .chunks(ternvale_acpi::LOADER_ENTRY_LEN)
        .map(|entry| ternvale_acpi::LoaderCommand::decode(entry).expect("decode"))
        .collect();
    assert_eq!(commands, blobs.commands);
    for (name, want) in [
        (ternvale_acpi::TABLES_FILE, &blobs.tables),
        (ternvale_acpi::RSDP_FILE, &blobs.rsdp),
    ] {
        let (item, size) = edk2.find_file(name).expect(name);
        edk2.select(item);
        assert_eq!(&edk2.read_bytes(size), want, "{name}");
    }
}
