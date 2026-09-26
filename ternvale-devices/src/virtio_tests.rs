use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use ternvale_vmm::{ExitEvent, GuestRegs, MmioBus, MmioError, VIRTIO_MMIO_BASE};

use super::{
    VirtioDevice, VirtioMmio, STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK,
    STATUS_FEATURES_OK,
};

struct Dummy {
    features: u64,
    config: u32,
    notified: Arc<Mutex<Vec<u16>>>,
    status: RefCell<Vec<u32>>,
}

impl Dummy {
    fn new(features: u64) -> Self {
        Self {
            features,
            config: 0x1122_3344,
            notified: Arc::new(Mutex::new(Vec::new())),
            status: RefCell::new(Vec::new()),
        }
    }
}

impl VirtioDevice for Dummy {
    fn device_id(&self) -> u32 {
        1
    }

    fn device_features(&self) -> u64 {
        self.features
    }

    fn read_config(&mut self, offset: u64, _size: u8) -> u64 {
        u64::from(self.config >> (offset * 8))
    }

    fn write_config(&mut self, offset: u64, size: u8, value: u64) {
        let shift = offset * 8;
        let mask = match size {
            1 => 0xffu32,
            2 => 0xffff,
            _ => 0xffff_ffff,
        };
        self.config = (self.config & !(mask << shift)) | ((value as u32 & mask) << shift);
    }

    fn notify(&mut self, queue: u16) {
        self.notified.lock().expect("notified").push(queue);
    }

    fn status_changed(&mut self, status: u32) {
        self.status.borrow_mut().push(status);
    }
}

fn transport() -> VirtioMmio {
    VirtioMmio::new(0, Box::new(Dummy::new(1)))
}

fn write_status(dev: &mut VirtioMmio, status: u32) {
    dev.write(0x070, 4, u64::from(status));
}

fn negotiate(dev: &mut VirtioMmio) {
    write_status(dev, STATUS_ACKNOWLEDGE);
    write_status(dev, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    dev.write(0x024, 4, 1);
    dev.write(0x020, 4, 1);
    dev.write(0x024, 4, 0);
    dev.write(0x020, 4, 1);
    write_status(dev, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK);
}

fn configure_queue(dev: &mut VirtioMmio) {
    dev.write(0x030, 4, 0);
    dev.write(0x038, 4, 8);
    dev.write(0x080, 4, 0x1000);
    dev.write(0x090, 4, 0x2000);
    dev.write(0x0a0, 4, 0x3000);
    dev.write(0x044, 4, 1);
}

#[test]
fn identity_registers_match_virtio_mmio_v2() {
    let mut dev = transport();
    assert_eq!(dev.read(0x000, 4), 0x7472_6976);
    assert_eq!(dev.read(0x004, 4), 2);
    assert_eq!(dev.read(0x008, 4), 1);
    assert_eq!(dev.read(0x00c, 4), 0x554d_4551);
    assert_eq!(dev.read(0x010, 4) & 1, 1);
    dev.write(0x014, 4, 1);
    assert_eq!(dev.read(0x010, 4) & 1, 1, "VERSION_1 is offered");
}

#[test]
fn features_ok_requires_version_1() {
    let mut dev = transport();
    write_status(&mut dev, STATUS_ACKNOWLEDGE);
    write_status(&mut dev, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    write_status(
        &mut dev,
        STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
    );
    assert_eq!(
        dev.read(0x070, 4),
        u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER)
    );
}

#[test]
fn handshake_reaches_driver_ok() {
    let mut dev = transport();
    negotiate(&mut dev);
    assert_eq!(
        dev.read(0x070, 4),
        u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK)
    );
    write_status(
        &mut dev,
        STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
    );
    assert_eq!(
        dev.read(0x070, 4),
        u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK)
    );
    assert_eq!(dev.read(0x014, 4), 0);
}

#[test]
fn out_of_order_status_is_rejected() {
    let dir = std::env::temp_dir().join(format!("ternvale-virtio-{}-status", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let mut config = ternvale_log::LogConfig::new("virtio", dir.clone());
    config.level = "error".to_string();
    let guard = ternvale_log::init(config).expect("log");
    let mut dev = transport();
    write_status(&mut dev, STATUS_DRIVER_OK);
    assert_eq!(dev.read(0x070, 4), 0);
    write_status(&mut dev, STATUS_ACKNOWLEDGE);
    write_status(
        &mut dev,
        STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
    );
    assert_eq!(dev.read(0x070, 4), u64::from(STATUS_ACKNOWLEDGE));
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    assert!(
        text.contains("virtio status rejected") && text.contains("out of order"),
        "{text}"
    );
    std::fs::remove_dir_all(&dir).expect("remove log dir");
}

#[test]
fn reset_clears_queues_and_status() {
    let mut dev = transport();
    negotiate(&mut dev);
    configure_queue(&mut dev);
    assert_eq!(dev.read(0x044, 4), 1);
    write_status(&mut dev, 0);
    assert_eq!(dev.read(0x070, 4), 0);
    assert_eq!(dev.read(0x044, 4), 0);
    dev.write(0x038, 4, 8);
    dev.write(0x044, 4, 1);
    assert_eq!(dev.read(0x044, 4), 0, "reset cleared the queue addresses");
    assert_eq!(dev.read(0x034, 4), 256);
}

#[test]
fn config_space_and_interrupt_ack() {
    let mut dev = transport();
    assert_eq!(dev.read(0x100, 4), 0x1122_3344);
    assert_eq!(dev.read(0x0fc, 4), 0);
    dev.write(0x100, 4, 0xaabb_ccdd);
    assert_eq!(dev.read(0x100, 4), 0xaabb_ccdd);
    dev.raise_interrupt(0b11);
    assert_eq!(dev.read(0x060, 4), 0b11);
    dev.write(0x064, 4, 0b01);
    assert_eq!(dev.read(0x060, 4), 0b10);
}

#[test]
fn notify_reaches_the_device_after_the_queue_is_ready() {
    let device = Dummy::new(1);
    let notified = device.notified.clone();
    let mut dev = VirtioMmio::new(0, Box::new(device));
    negotiate(&mut dev);
    dev.write(0x050, 4, 0);
    assert!(notified.lock().expect("notified").is_empty());
    configure_queue(&mut dev);
    dev.write(0x050, 4, 0);
    assert_eq!(notified.lock().expect("notified").as_slice(), &[0]);
}

#[test]
fn bus_read_of_slot_zero_returns_magic() {
    let mut bus = MmioBus::new();
    VirtioMmio::register(&mut bus, 0, Box::new(Dummy::new(0))).expect("register");
    let regs = Regs(RefCell::new([0; 31]));
    bus.dispatch(
        &regs,
        ExitEvent::Mmio {
            gpa: VIRTIO_MMIO_BASE,
            size: 4,
            write: false,
            reg: 0,
        },
    )
    .expect("dispatch");
    assert_eq!(regs.0.borrow()[0], 0x7472_6976);
    assert_eq!(bus.access_count("virtio-mmio-0"), Some(1));
    let err = VirtioMmio::register(&mut bus, 32, Box::new(Dummy::new(0))).unwrap_err();
    assert!(err.to_string().contains("slot 32"));
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
