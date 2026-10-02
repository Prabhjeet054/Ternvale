//! virtio-pci through the real ECAM and BAR windows on an `MmioBus`, no VM.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use ternvale_vmm::pci::{LineHook, PciRoot, PCI_INTX_LINES};
use ternvale_vmm::{ExitEvent, GuestRegs, MmioBus, MmioError, PCIE_ECAM_BASE};

use super::caps::{DEVICE_OFFSET, ISR_OFFSET, NOTIFY_OFFSET};
use super::VirtioPci;
use crate::virtio::irq::VirtioIrq;
use crate::virtio::{
    QueueNotify, VirtioDevice, STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK,
    STATUS_FEATURES_OK,
};

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

/// Guest view of one bus with the PCI root registered; device 1 is under test.
pub(super) struct Guest {
    bus: MmioBus,
    regs: Regs,
    pub(super) levels: Arc<Mutex<Vec<(usize, bool)>>>,
    pub(super) bar0: u64,
}

impl Guest {
    /// Root with `build`'s function on 00:01.0, BARs assigned, decode enabled.
    pub(super) fn new(
        build: impl FnOnce(ternvale_vmm::pci::Bdf, Arc<ternvale_vmm::pci::IntxPin>) -> VirtioPci,
    ) -> Self {
        let levels = Arc::new(Mutex::new(Vec::new()));
        let hooks: [LineHook; PCI_INTX_LINES] = std::array::from_fn(|i| {
            let levels = Arc::clone(&levels);
            Arc::new(move |level| levels.lock().expect("levels").push((i, level))) as LineHook
        });
        let root = PciRoot::new(hooks);
        root.add(|bdf, pin| Ok(Box::new(build(bdf, pin))))
            .expect("add");
        let mut bus = MmioBus::new();
        root.register(&mut bus).expect("register");
        let mut guest = Self {
            bus,
            regs: Regs(RefCell::new([0; 31])),
            levels,
            bar0: 0,
        };
        guest.bar0 = guest.cfg(0x10, 4) & !0xf;
        guest.set_cfg(0x04, 2, 0x0006);
        guest
    }

    fn access(&self, gpa: u64, size: u8, write: bool) {
        let event = ExitEvent::Mmio {
            gpa,
            size,
            write,
            reg: 1,
        };
        self.bus.dispatch(&self.regs, event).expect("dispatch");
    }

    pub(super) fn read(&self, gpa: u64, size: u8) -> u64 {
        self.access(gpa, size, false);
        self.regs.0.borrow()[1]
    }

    pub(super) fn write(&self, gpa: u64, size: u8, value: u64) {
        self.regs.0.borrow_mut()[1] = value;
        self.access(gpa, size, true);
    }

    pub(super) fn cfg(&self, reg: u64, size: u8) -> u64 {
        self.read(PCIE_ECAM_BASE + (1 << 15) + reg, size)
    }

    pub(super) fn set_cfg(&self, reg: u64, size: u8, value: u64) {
        self.write(PCIE_ECAM_BASE + (1 << 15) + reg, size, value);
    }

    pub(super) fn common(&self, offset: u64, size: u8) -> u64 {
        self.read(self.bar0 + offset, size)
    }

    pub(super) fn set_common(&self, offset: u64, size: u8, value: u64) {
        self.write(self.bar0 + offset, size, value);
    }

    /// Reset, then ACK, DRIVER, VERSION_1, FEATURES_OK.
    pub(super) fn negotiate(&self) {
        self.set_common(0x14, 1, 0);
        self.set_common(0x14, 1, u64::from(STATUS_ACKNOWLEDGE));
        self.set_common(0x14, 1, u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER));
        self.set_common(0x08, 4, 1);
        self.set_common(0x0c, 4, 1);
        self.set_common(
            0x14,
            1,
            u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK),
        );
    }

    /// Queue 0 at `desc`/`avail`/`used` with `size` entries, then enable it.
    pub(super) fn setup_queue(&self, size: u16, desc: u64, avail: u64, used: u64) {
        self.set_common(0x16, 2, 0);
        self.set_common(0x18, 2, u64::from(size));
        self.set_common(0x20, 4, desc & 0xffff_ffff);
        self.set_common(0x24, 4, desc >> 32);
        self.set_common(0x28, 8, avail);
        self.set_common(0x30, 4, used);
        self.set_common(0x1c, 2, 1);
    }

    pub(super) fn line_levels(&self) -> Vec<(usize, bool)> {
        self.levels.lock().expect("levels").clone()
    }
}

struct Dummy {
    notified: Arc<Mutex<Vec<u16>>>,
}

impl VirtioDevice for Dummy {
    fn device_id(&self) -> u32 {
        4
    }

    fn device_features(&self) -> u64 {
        1 << 3
    }

    fn read_config(&mut self, offset: u64, _size: u8) -> u64 {
        u64::from(0x1122_3344u32 >> (offset * 8))
    }

    fn notify(&mut self, queue: QueueNotify) {
        self.notified.lock().expect("notified").push(queue.index);
    }
}

fn dummy() -> (Guest, Arc<VirtioIrq>, Arc<Mutex<Vec<u16>>>) {
    let notified = Arc::new(Mutex::new(Vec::new()));
    let irq = VirtioIrq::new();
    let (n, i) = (Arc::clone(&notified), Arc::clone(&irq));
    let guest = Guest::new(move |bdf, pin| {
        VirtioPci::new(bdf, Box::new(Dummy { notified: n }), i, pin, 0).expect("virtio-pci")
    });
    (guest, irq, notified)
}

#[test]
fn identity_matches_modern_virtio_pci() {
    let (guest, _, _) = dummy();
    assert_eq!(
        guest.cfg(0x00, 4),
        0x1044_1af4,
        "vendor 0x1af4, device 0x1040 + 4"
    );
    assert_eq!(guest.cfg(0x08, 1), 1, "revision 1 (modern)");
    assert_eq!(guest.cfg(0x2c, 2), 0x1af4);
    assert_eq!(guest.cfg(0x06, 2) & 0x10, 0x10, "capability list");
    assert_eq!(guest.cfg(0x3d, 1), 1, "INTA");
    assert_eq!(guest.bar0, ternvale_vmm::PCIE_MMIO_BASE);
    guest.set_cfg(0x04, 2, 0);
    guest.set_cfg(0x10, 4, 0xffff_ffff);
    assert_eq!(guest.cfg(0x10, 4), 0xffff_c000, "16 KiB 32-bit bar");
}

#[test]
fn capability_list_describes_bar0_layout() {
    let (guest, _, _) = dummy();
    let mut caps = Vec::new();
    let mut at = guest.cfg(0x34, 1);
    while at != 0 {
        assert_eq!(guest.cfg(at, 1), 0x09, "vendor capability");
        let cfg_type = guest.cfg(at + 3, 1);
        let fields = (
            cfg_type,
            guest.cfg(at + 4, 1),
            guest.cfg(at + 8, 4),
            guest.cfg(at + 12, 4),
        );
        caps.push(fields);
        if cfg_type == 2 {
            assert_eq!(guest.cfg(at + 2, 1), 20, "notify cap_len");
            assert_eq!(guest.cfg(at + 16, 4), 4, "notify_off_multiplier");
        }
        at = guest.cfg(at + 1, 1);
    }
    assert_eq!(
        caps,
        vec![
            (1, 0, 0, 0x1000),
            (2, 0, 0x3000, 0x1000),
            (3, 0, 0x1000, 0x1000),
            (4, 0, 0x2000, 0x1000),
            (5, 0, 0, 0)
        ]
    );
}

#[test]
fn msix_appends_a_capability_and_doubles_bar0() {
    let irq = VirtioIrq::new();
    let guest = Guest::new(move |bdf, pin| {
        let device = Box::new(Dummy {
            notified: Arc::default(),
        });
        VirtioPci::new(bdf, device, irq, pin, 2).expect("virtio-pci")
    });
    let mut ids = Vec::new();
    let mut at = guest.cfg(0x34, 1) & !3;
    while at != 0 {
        assert!(ids.len() < 48, "capability walk did not terminate");
        assert!(at >= 0x40, "capability {at:#x} points into the header");
        ids.push((guest.cfg(at, 1), guest.cfg(at + 3, 1)));
        at = guest.cfg(at + 1, 1) & !3;
    }
    let types: Vec<u64> = ids.iter().map(|&(_, t)| t).collect();
    assert_eq!(
        &types[..5],
        &[1, 2, 3, 4, 5],
        "common, notify, isr, device, pci_cfg"
    );
    assert!(
        ids[..5].iter().all(|&(id, _)| id == 0x09),
        "vendor capabilities"
    );
    assert_eq!(ids[5].0, 0x11, "MSI-X last");
    assert_eq!(ids.len(), 6);
    guest.set_cfg(0x04, 2, 0);
    guest.set_cfg(0x10, 4, 0xffff_ffff);
    assert_eq!(
        guest.cfg(0x10, 4),
        0xffff_8000,
        "32 KiB with the MSI-X table"
    );
}

#[test]
fn handshake_queue_setup_and_notify_through_bar0() {
    let (guest, _, notified) = dummy();
    guest.set_common(0x00, 4, 1);
    assert_eq!(guest.common(0x04, 4), 1, "VERSION_1 offered in word 1");
    guest.set_common(0x00, 4, 0);
    assert_eq!(guest.common(0x04, 4), 1 << 3);
    assert_eq!(guest.common(0x12, 2), 1, "num_queues");
    guest.negotiate();
    assert_eq!(
        guest.common(0x14, 1),
        u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK)
    );
    guest.set_common(0x16, 2, 0);
    assert_eq!(
        guest.common(0x18, 2),
        256,
        "queue_size reports the maximum first"
    );
    assert_eq!(guest.common(0x1e, 2), 0, "queue_notify_off");
    guest.write(guest.bar0 + NOTIFY_OFFSET, 2, 0);
    assert!(
        notified.lock().expect("n").is_empty(),
        "queue not enabled yet"
    );
    guest.setup_queue(8, 0x4000_1000, 0x4000_2000, 0x4000_3000);
    assert_eq!(guest.common(0x1c, 2), 1);
    assert_eq!(guest.common(0x18, 2), 8);
    assert_eq!(guest.common(0x20, 8), 0x4000_1000);
    assert_eq!(guest.common(0x2c, 4), 0, "queue_driver high half");
    guest.set_common(
        0x14,
        1,
        u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK),
    );
    guest.write(guest.bar0 + NOTIFY_OFFSET, 2, 0);
    assert_eq!(*notified.lock().expect("n"), vec![0]);
    guest.set_common(0x14, 1, 0);
    assert_eq!(guest.common(0x14, 1), 0, "reset");
    assert_eq!(guest.common(0x1c, 2), 0, "reset clears queue_enable");
}

#[test]
fn isr_read_clears_and_drops_intx() {
    let (guest, irq, _) = dummy();
    irq.raise(1);
    assert_eq!(
        guest.line_levels(),
        vec![(1, true)],
        "00:01.0 INTA is line 1"
    );
    assert_eq!(guest.cfg(0x06, 2) & 0x08, 0x08, "interrupt status");
    assert_eq!(guest.read(guest.bar0 + ISR_OFFSET, 1), 1);
    assert_eq!(guest.line_levels(), vec![(1, true), (1, false)]);
    assert_eq!(guest.read(guest.bar0 + ISR_OFFSET, 1), 0, "read-to-clear");
    guest.set_cfg(0x04, 2, 0x0406);
    irq.raise(2);
    assert_eq!(guest.line_levels().len(), 2, "INTx disable masks the pin");
    assert_eq!(guest.read(guest.bar0 + ISR_OFFSET, 1), 2);
}

#[test]
fn device_cfg_and_pci_cfg_window_reach_the_device() {
    let (guest, _, _) = dummy();
    assert_eq!(guest.read(guest.bar0 + DEVICE_OFFSET, 4), 0x1122_3344);
    let mut cap = guest.cfg(0x34, 1);
    while guest.cfg(cap + 3, 1) != 5 {
        cap = guest.cfg(cap + 1, 1);
    }
    guest.set_cfg(cap + 4, 1, 0);
    guest.set_cfg(cap + 8, 4, DEVICE_OFFSET);
    guest.set_cfg(cap + 12, 4, 4);
    assert_eq!(guest.cfg(cap + 16, 4), 0x1122_3344);
    guest.set_cfg(cap + 8, 4, 0x12);
    guest.set_cfg(cap + 12, 4, 2);
    assert_eq!(guest.cfg(cap + 16, 4), 1, "num_queues through pci_cfg");
    guest.set_cfg(cap + 12, 4, 3);
    assert_eq!(guest.cfg(cap + 16, 4), 0, "bad length is refused");
}

#[test]
fn msix_vectors_read_back_no_vector_without_msix() {
    let (guest, _, _) = dummy();
    guest.set_common(0x10, 2, 0);
    assert_eq!(guest.common(0x10, 2), 0xffff);
    guest.set_common(0x1a, 2, 0);
    assert_eq!(guest.common(0x1a, 2), 0xffff);
    assert_eq!(guest.common(0x14, 4), 0, "mismatched access size reads 0");
}
