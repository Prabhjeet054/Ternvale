//! virtio-gpu through the real transports on an `MmioBus`, no VM: a
//! virtio-mmio slot and a virtio-pci function behind the ECAM window.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_devices::{
    VirtioIrq, VirtioPci, STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK, STATUS_FEATURES_OK,
};
use ternvale_vmm::pci::{LineHook, PciRoot, PCI_INTX_LINES};
use ternvale_vmm::{ExitEvent, GuestMemory, GuestRegs, MmioBus, MmioError, PCIE_ECAM_BASE};

use super::*;
use crate::protocol::{self, CMD_UPDATE_CURSOR, RESP_OK_DISPLAY_INFO, RESP_OK_NODATA};
use crate::testutil::{self, rect, Driver, FORMAT_BGRX, HEAP, QUEUE_SIZE};

const SLOT: u32 = 5;
const FEATURES_OK: u32 = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;

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

/// Guest accesses through one bus.
struct Bus {
    bus: MmioBus,
    regs: Regs,
}

impl Bus {
    fn new(bus: MmioBus) -> Self {
        Self {
            bus,
            regs: Regs(RefCell::new([0; 31])),
        }
    }

    fn read(&self, gpa: u64, size: u8) -> u64 {
        let event = ExitEvent::Mmio {
            gpa,
            size,
            write: false,
            reg: 1,
        };
        self.bus.dispatch(&self.regs, event).expect("read");
        self.regs.0.borrow()[1]
    }

    fn write(&self, gpa: u64, size: u8, value: u64) {
        self.regs.0.borrow_mut()[1] = value;
        let event = ExitEvent::Mmio {
            gpa,
            size,
            write: true,
            reg: 1,
        };
        self.bus.dispatch(&self.regs, event).expect("write");
    }
}

/// Send one request on `driver`'s queue, ring `kick`, wait for the answer.
fn exchange(driver: &mut Driver, kick: &dyn Fn(u16), request: &[u8], response_len: u32) -> Vec<u8> {
    driver.publish(request, response_len);
    kick(driver.queue);
    driver.wait_response()
}

/// The guest side of a framebuffer: display info, then a 64×32 resource with
/// the test pattern shown on scanout 0 and flushed.
fn draw_pattern(driver: &mut Driver, kick: &dyn Fn(u16), memory: &Arc<Mutex<GuestMemory>>) {
    let info = exchange(
        driver,
        kick,
        &testutil::get_display_info(),
        protocol::DISPLAY_INFO_LEN as u32,
    );
    assert_eq!(testutil::kind(&info), RESP_OK_DISPLAY_INFO);
    assert_eq!(info[32..36], 1280u32.to_le_bytes(), "scanout 0 width");
    let pages = [(HEAP + 0x2000, 4096), (HEAP, 4096)];
    testutil::fill_backing(memory, &pages, &testutil::image(64, 32, 0x66));
    for request in [
        testutil::create_2d(1, FORMAT_BGRX, 64, 32),
        testutil::attach_backing(1, &pages),
        testutil::transfer(rect(0, 0, 64, 32), 0, 1),
        testutil::set_scanout(rect(0, 0, 64, 32), 0, 1),
        testutil::flush(rect(0, 0, 64, 32), 1),
    ] {
        let answer = exchange(driver, kick, &request, 24);
        assert_eq!(
            testutil::kind(&answer),
            RESP_OK_NODATA,
            "{}",
            protocol::type_name(testutil::kind(&request))
        );
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn config_validation_rejects_bad_scanout_lists() {
    assert!(GpuConfig::default().validate().is_ok());
    assert!(GpuConfig { modes: vec![] }.validate().is_err());
    assert!(GpuConfig {
        modes: vec![(640, 480); 17]
    }
    .validate()
    .is_err());
    assert!(GpuConfig {
        modes: vec![(0, 480)]
    }
    .validate()
    .is_err());
    assert!(GpuConfig {
        modes: vec![(MAX_DIMENSION + 1, 480)]
    }
    .validate()
    .is_err());
    let irq = VirtioIrq::new();
    let error = VirtioGpu::new(GpuConfig { modes: vec![] }, testutil::memory(), irq)
        .err()
        .expect("refused");
    assert!(error.to_string().contains("0 scanouts"), "{error}");
    let hook: ternvale_devices::IrqHook = Arc::new(|_| {});
    assert!(matches!(
        VirtioGpu::attach(99, GpuConfig::default(), testutil::memory(), hook),
        Err(GpuError::BadSlot { slot: 99 })
    ));
}

#[test]
fn virtio_mmio_identity_config_and_a_frame_end_to_end() {
    let memory = testutil::memory();
    let raised = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&raised);
    let hook: ternvale_devices::IrqHook =
        Arc::new(move |level| flag.store(level, Ordering::Release));
    let gpu =
        VirtioGpu::attach(SLOT, GpuConfig::default(), Arc::clone(&memory), hook).expect("attach");
    let (display, stats) = (gpu.display.clone(), Arc::clone(&gpu.stats));
    let mut bus = MmioBus::new();
    bus.register(gpu.base, gpu.size, gpu.device)
        .expect("register");
    let bus = Bus::new(bus);
    let base = gpu.base;
    let reg = |offset: u64| bus.read(base + offset, 4);
    let set = |offset: u64, value: u64| bus.write(base + offset, 4, value);

    assert_eq!(reg(0x000), 0x7472_6976, "magic");
    assert_eq!(reg(0x004), 2, "virtio-mmio v2");
    assert_eq!(reg(0x008), u64::from(VIRTIO_GPU_ID));
    set(0x014, 0);
    assert_eq!(reg(0x010), 0, "no 2D-profile feature bits");
    set(0x014, 1);
    assert_eq!(reg(0x010), 1, "VIRTIO_F_VERSION_1");
    for (queue, max) in [(0, 256), (1, 256), (2, 0)] {
        set(0x030, queue);
        assert_eq!(reg(0x034), max, "queue {queue} max size");
    }
    assert_eq!(
        [reg(0x100), reg(0x104), reg(0x108), reg(0x10c)],
        [0, 0, 1, 0],
        "events_read, events_clear, num_scanouts, num_capsets"
    );
    assert_eq!(bus.read(base + 0x108, 1), 1, "byte access to num_scanouts");

    set(0x070, u64::from(STATUS_ACKNOWLEDGE));
    set(0x070, u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER));
    set(0x024, 1);
    set(0x020, 1);
    set(0x070, u64::from(FEATURES_OK));
    assert_eq!(reg(0x070), u64::from(FEATURES_OK));
    let mut control = Driver::new(Arc::clone(&memory), 0);
    let mut cursor = Driver::new(Arc::clone(&memory), 1);
    for driver in [&control, &cursor] {
        set(0x030, u64::from(driver.queue));
        set(0x038, u64::from(QUEUE_SIZE));
        for (offset, addr) in [
            (0x080, driver.desc_addr()),
            (0x090, driver.avail_addr()),
            (0x0a0, driver.used_addr()),
        ] {
            set(offset, addr & 0xffff_ffff);
            set(offset + 4, addr >> 32);
        }
        set(0x044, 1);
    }
    set(0x070, u64::from(FEATURES_OK | STATUS_DRIVER_OK));
    let kick = |queue: u16| set(0x050, u64::from(queue));

    draw_pattern(&mut control, &kick, &memory);
    let frame = display.frame(0).expect("frame");
    assert_eq!(
        frame.pixels,
        testutil::image(64, 32, 0x66),
        "host scanout buffer holds the guest's pattern"
    );
    assert!(
        raised.load(Ordering::Acquire),
        "used-buffer interrupt raised"
    );
    assert_eq!(reg(0x060) & 1, 1, "InterruptStatus used-buffer bit");
    assert_eq!(stats.snapshot()[..3], [6, 0, 1]);

    // Cursor commands are taken off the queue with no response.
    assert!(exchange(
        &mut cursor,
        &kick,
        &testutil::hdr(CMD_UPDATE_CURSOR, 0, 0),
        0
    )
    .is_empty());

    set(0x070, 0);
    wait_until("reset to blank the scanout", || {
        display.frame(0).is_some_and(|f| f.format.is_none())
    });
}

#[test]
fn virtio_pci_identity_class_and_a_frame_end_to_end() {
    let memory = testutil::memory();
    let hooks: [LineHook; PCI_INTX_LINES] = std::array::from_fn(|_| Arc::new(|_| {}) as LineHook);
    let root = PciRoot::new(hooks);
    let irq = VirtioIrq::new();
    let gpu = VirtioGpu::new(
        GpuConfig {
            modes: vec![(1280, 800), (800, 600)],
        },
        Arc::clone(&memory),
        Arc::clone(&irq),
    )
    .expect("gpu");
    let display = gpu.display();
    let bdf = root
        .add(move |bdf, pin| {
            Ok(Box::new(
                VirtioPci::new(bdf, Box::new(gpu), irq, pin, 0).map_err(|e| e.to_string())?,
            ))
        })
        .expect("add");
    assert_eq!(bdf.to_string(), "00:01.0");
    let mut bus = MmioBus::new();
    root.register(&mut bus).expect("register");
    let bus = Bus::new(bus);
    let cfg = |reg: u64| bus.read(PCIE_ECAM_BASE + (1 << 15) + reg, 4);
    assert_eq!(
        cfg(0x00),
        0x1050_1af4,
        "virtio-gpu is PCI device 0x1040 + 16"
    );
    assert_eq!(cfg(0x08) >> 8, 0x03_80_00, "display controller, other");
    let bar0 = cfg(0x10) & !0xf;
    bus.write(PCIE_ECAM_BASE + (1 << 15) + 0x04, 2, 0x0006);
    let common = |offset: u64, size: u8, value: u64| bus.write(bar0 + offset, size, value);
    assert_eq!(
        bus.read(bar0 + 0x2000 + 8, 4),
        2,
        "num_scanouts in the device-config BAR window"
    );
    assert_eq!(bus.read(bar0 + 0x12, 2), 2, "num_queues");

    common(0x14, 1, u64::from(STATUS_ACKNOWLEDGE));
    common(0x14, 1, u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER));
    common(0x08, 4, 1);
    common(0x0c, 4, 1);
    common(0x14, 1, u64::from(FEATURES_OK));
    let mut control = Driver::new(Arc::clone(&memory), 0);
    common(0x16, 2, 0);
    common(0x18, 2, u64::from(QUEUE_SIZE));
    for (offset, addr) in [
        (0x20, control.desc_addr()),
        (0x28, control.avail_addr()),
        (0x30, control.used_addr()),
    ] {
        common(offset, 4, addr & 0xffff_ffff);
        common(offset + 4, 4, addr >> 32);
    }
    common(0x1c, 2, 1);
    common(0x14, 1, u64::from(FEATURES_OK | STATUS_DRIVER_OK));
    let kick = |queue: u16| bus.write(bar0 + 0x3000 + u64::from(queue) * 4, 2, u64::from(queue));

    let info = exchange(
        &mut control,
        &kick,
        &testutil::get_display_info(),
        protocol::DISPLAY_INFO_LEN as u32,
    );
    assert_eq!(
        info[56..64],
        [800u32.to_le_bytes(), 600u32.to_le_bytes()].concat(),
        "scanout 1 mode"
    );
    draw_pattern(&mut control, &kick, &memory);
    assert_eq!(
        display.frame(0).expect("frame").pixels,
        testutil::image(64, 32, 0x66)
    );
    assert_eq!(
        display.frame(1).expect("frame").format,
        None,
        "scanout 1 never set"
    );
}
