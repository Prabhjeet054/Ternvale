use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use super::{
    Bar, BarKind, ConfigSpace, Header, IntxPin, LineHook, PciError, PciFunction, PciRoot,
    COMMAND_INTX_DISABLE, COMMAND_MEMORY, HOST_BRIDGE_DEVICE_ID, HOST_BRIDGE_VENDOR_ID,
    PCI_INTX_LINES,
};
use crate::esr::ExitEvent;
use crate::mmio::{GuestRegs, MmioBus, MmioError};
use crate::platform::{PCIE_ECAM_BASE, PCIE_MMIO_BASE};

pub(super) struct Regs(RefCell<[u64; 31]>);

impl GuestRegs for Regs {
    fn get_reg(&self, index: u8) -> Result<u64, MmioError> {
        Ok(self.0.borrow()[usize::from(index)])
    }

    fn set_reg(&self, index: u8, value: u64) -> Result<(), MmioError> {
        self.0.borrow_mut()[usize::from(index)] = value;
        Ok(())
    }
}

/// Guest view of an `MmioBus` with the PCI root registered, no VM.
pub(super) struct Guest {
    bus: MmioBus,
    regs: Regs,
}

impl Guest {
    /// Register `root` (assigning BARs) on a fresh bus.
    pub(super) fn attach(root: &Arc<PciRoot>) -> Self {
        let mut bus = MmioBus::new();
        root.register(&mut bus).expect("register");
        Self {
            bus,
            regs: Regs(RefCell::new([0; 31])),
        }
    }

    pub(super) fn read(&self, gpa: u64, size: u8) -> u64 {
        let event = ExitEvent::Mmio {
            gpa,
            size,
            write: false,
            reg: 1,
        };
        self.bus.dispatch(&self.regs, event).expect("read");
        self.regs.0.borrow()[1]
    }

    pub(super) fn write(&self, gpa: u64, size: u8, value: u64) {
        self.regs.0.borrow_mut()[2] = value;
        let event = ExitEvent::Mmio {
            gpa,
            size,
            write: true,
            reg: 2,
        };
        self.bus.dispatch(&self.regs, event).expect("write");
    }

    pub(super) fn cfg_read(&self, device: u64, reg: u64, size: u8) -> u64 {
        self.read(PCIE_ECAM_BASE + (device << 15) + reg, size)
    }

    pub(super) fn cfg_write(&self, device: u64, reg: u64, size: u8, value: u64) {
        self.write(PCIE_ECAM_BASE + (device << 15) + reg, size, value);
    }
}

type Log = Arc<Mutex<Vec<(usize, u64, u8, u64)>>>;

struct Probe {
    config: ConfigSpace,
    writes: Log,
}

impl PciFunction for Probe {
    fn name(&self) -> &str {
        "probe"
    }

    fn config(&self) -> &ConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut ConfigSpace {
        &mut self.config
    }

    fn read_bar(&mut self, bar: usize, offset: u64, _size: u8) -> u64 {
        0x1000 * bar as u64 + offset
    }

    fn write_bar(&mut self, bar: usize, offset: u64, size: u8, value: u64) {
        self.writes
            .lock()
            .expect("writes")
            .push((bar, offset, size, value));
    }
}

fn probe(writes: Log) -> Box<dyn PciFunction> {
    let mut config = ConfigSpace::new(Header {
        vendor_id: 0x1234,
        device_id: 0x5678,
        class_code: 0xff_00_00,
        revision: 0,
        subsystem_vendor_id: 0,
        subsystem_id: 0,
        interrupt_pin: 1,
    });
    let bar = |size| Bar {
        size,
        kind: BarKind::Mem32,
        prefetchable: false,
    };
    config.add_bar(0, bar(0x1000)).expect("bar0");
    config.add_bar(1, bar(0x10_0000)).expect("bar1");
    Box::new(Probe { config, writes })
}

pub(super) type Levels = Arc<Mutex<Vec<(usize, bool)>>>;

pub(super) fn setup() -> (Guest, Arc<PciRoot>, Levels) {
    let levels = Arc::new(Mutex::new(Vec::new()));
    let hooks: [LineHook; PCI_INTX_LINES] = std::array::from_fn(|i| {
        let levels = Arc::clone(&levels);
        Arc::new(move |level| levels.lock().expect("levels").push((i, level))) as LineHook
    });
    let root = PciRoot::new(hooks);
    let mut bus = MmioBus::new();
    root.register(&mut bus).expect("register");
    (
        Guest {
            bus,
            regs: Regs(RefCell::new([0; 31])),
        },
        root,
        levels,
    )
}

#[test]
fn ecam_reads_host_bridge_and_all_ones_for_absent_functions() {
    let (guest, _root, _) = setup();
    let ids = u64::from(HOST_BRIDGE_DEVICE_ID) << 16 | u64::from(HOST_BRIDGE_VENDOR_ID);
    assert_eq!(guest.cfg_read(0, 0, 4), ids);
    assert_eq!(
        guest.cfg_read(0, 0x08, 4) >> 8,
        0x06_00_00,
        "host bridge class"
    );
    assert_eq!(guest.cfg_read(0, 0x0a, 2), 0x0600);
    assert_eq!(guest.cfg_read(1, 0, 4), 0xffff_ffff, "no device 1");
    assert_eq!(guest.cfg_read(0, 0x02, 2), u64::from(HOST_BRIDGE_DEVICE_ID));
    assert_eq!(
        guest.read(PCIE_ECAM_BASE + (1 << 12), 4),
        0xffff_ffff,
        "function 1"
    );
    assert_eq!(guest.read(PCIE_ECAM_BASE + (1 << 20), 2), 0xffff, "bus 1");
    assert_eq!(guest.read(PCIE_ECAM_BASE + 1, 2), 0xffff, "misaligned");
    assert_eq!(guest.cfg_read(0, 0x100, 4), 0, "extended space");
}

#[test]
fn bars_are_preassigned_sized_and_routed_once_decode_is_on() {
    let (_g, root, _) = setup();
    let writes: Log = Arc::default();
    let log = Arc::clone(&writes);
    let bdf = root.add(move |_, _| Ok(probe(log))).expect("add");
    assert_eq!(bdf.device, 1);
    let mut bus = MmioBus::new();
    root.register(&mut bus).expect("register");
    let guest = Guest {
        bus,
        regs: Regs(RefCell::new([0; 31])),
    };
    assert_eq!(guest.cfg_read(1, 0, 4), 0x5678_1234);
    let bar1 = PCIE_MMIO_BASE;
    let bar0 = PCIE_MMIO_BASE + 0x10_0000;
    assert_eq!(guest.cfg_read(1, 0x14, 4), bar1, "largest bar first");
    assert_eq!(guest.cfg_read(1, 0x10, 4), bar0);
    assert_eq!(guest.read(bar0, 4), u64::MAX & 0xffff_ffff, "decode off");
    guest.cfg_write(1, 0x04, 2, u64::from(COMMAND_MEMORY));
    assert_eq!(guest.read(bar0 + 8, 4), 8);
    assert_eq!(guest.read(bar1 + 0x20, 4), 0x1020);
    guest.write(bar0 + 4, 2, 0xabcd);
    assert_eq!(*writes.lock().expect("writes"), vec![(0, 4, 2, 0xabcd)]);
    // Sizing protocol as Linux runs it: decode off, all-ones, read, restore.
    guest.cfg_write(1, 0x04, 2, 0);
    guest.cfg_write(1, 0x10, 4, 0xffff_ffff);
    assert_eq!(guest.cfg_read(1, 0x10, 4), 0xffff_f000);
    guest.cfg_write(1, 0x10, 4, 0x2000_0000);
    guest.cfg_write(1, 0x04, 2, u64::from(COMMAND_MEMORY));
    assert_eq!(guest.read(0x2000_0010, 4), 0x10, "routed to the new base");
    assert_eq!(
        guest.read(bar0, 4),
        0xffff_ffff,
        "old base no longer decodes"
    );
}

#[test]
fn intx_follows_pin_status_and_disable_bit() {
    let (_g, root, levels) = setup();
    let pin: Arc<Mutex<Option<Arc<IntxPin>>>> = Arc::default();
    let slot = Arc::clone(&pin);
    root.add(move |_, p| {
        *slot.lock().expect("pin") = Some(p);
        Ok(probe(Arc::default()))
    })
    .expect("add");
    let mut bus = MmioBus::new();
    root.register(&mut bus).expect("register");
    let guest = Guest {
        bus,
        regs: Regs(RefCell::new([0; 31])),
    };
    let pin = pin.lock().expect("pin").clone().expect("pin set");
    assert_eq!(pin.spi(), 4, "device 1 INTA swizzles onto line 1 (SPI 4)");
    pin.set(true);
    assert_eq!(*levels.lock().expect("levels"), vec![(1, true)]);
    assert_eq!(
        guest.cfg_read(1, 0x06, 2) & 0x08,
        0x08,
        "status interrupt bit"
    );
    assert_eq!(
        guest.cfg_read(1, 0x04, 4) >> 16 & 0x08,
        0x08,
        "dword read too"
    );
    guest.cfg_write(1, 0x04, 2, u64::from(COMMAND_INTX_DISABLE));
    assert_eq!(*levels.lock().expect("levels"), vec![(1, true), (1, false)]);
    pin.set(false);
    assert_eq!(guest.cfg_read(1, 0x06, 2) & 0x08, 0);
}

#[test]
fn add_reports_builder_errors_and_a_full_bus() {
    let (_g, root, _) = setup();
    let err = root.add(|_, _| Err("boom".to_string())).unwrap_err();
    assert!(matches!(err, PciError::Build { .. }), "{err}");
    for _ in 1..32 {
        root.add(|_, _| Ok(probe(Arc::default()))).expect("add");
    }
    assert!(matches!(
        root.add(|_, _| Ok(probe(Arc::default()))),
        Err(PciError::BusFull)
    ));
}
