use std::cell::RefCell;

use super::{Pl011, PL011_BASE, PL011_SIZE};
use ternvale_vmm::{ExitEvent, GuestRegs, MmioBus, MmioError};

fn uart(sink: Vec<u8>) -> (Pl011, std::rc::Rc<RefCell<Vec<u8>>>) {
    let sink = std::rc::Rc::new(RefCell::new(sink));
    let shared = std::rc::Rc::clone(&sink);
    let device = Pl011::new(Box::new(Share(sink)));
    (device, shared)
}

struct Share(std::rc::Rc<RefCell<Vec<u8>>>);

impl super::ByteSink for Share {
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(())
    }
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

#[test]
fn tx_bytes_reach_the_sink_and_rx_flags_follow_the_queue() {
    let (mut uart, sink) = uart(Vec::new());
    uart.write(0, 1, u64::from(b'A'));
    uart.write(0, 1, u64::from(b'\n'));
    assert_eq!(sink.borrow().as_slice(), b"A\n");
    let fr = uart.read(0x18, 4);
    assert_eq!(fr & (1 << 5), 0, "TXFF clear");
    assert_ne!(fr & (1 << 4), 0, "RXFE set");
    assert!(uart.push_rx(b'Z'));
    let fr = uart.read(0x18, 4);
    assert_eq!(fr & (1 << 4), 0, "RXFE clear");
    assert_ne!(fr & (1 << 6), 0, "RXFF set for a 1-byte fifo");
    assert!(!uart.push_rx(b'Y'));
    assert_eq!(uart.read(0, 1), u64::from(b'Z'));
    assert_ne!(uart.read(0x18, 4) & (1 << 4), 0);
}

#[test]
fn periph_id_matches_the_arm_pl011() {
    let (mut uart, _) = uart(Vec::new());
    let id: Vec<u64> = (0..8)
        .map(|index| uart.read(0xfe0 + index * 4, 4))
        .collect();
    assert_eq!(id, vec![0x11, 0x10, 0x14, 0x00, 0x0d, 0xf0, 0x05, 0xb1]);
}

#[test]
fn write_to_a_read_only_register_is_ignored() {
    let (mut uart, _) = uart(Vec::new());
    uart.write(0xfe0, 4, 0xff);
    assert_eq!(uart.read(0xfe0, 4), 0x11);
    uart.write(0x18, 4, 0);
    assert_ne!(uart.read(0x18, 4) & (1 << 7), 0, "TXFE stays set");
}

#[test]
fn bus_write_to_uartdr_delivers_the_byte() {
    let (device, sink) = uart(Vec::new());
    let mut bus = MmioBus::new();
    bus.register(PL011_BASE, PL011_SIZE, Box::new(device))
        .expect("register");
    let regs = Regs(RefCell::new([0; 31]));
    regs.0.borrow_mut()[0] = u64::from(b'H');
    bus.dispatch(
        &regs,
        ExitEvent::Mmio {
            gpa: PL011_BASE,
            size: 1,
            write: true,
            reg: 0,
        },
    )
    .expect("dispatch");
    assert_eq!(sink.borrow().as_slice(), b"H");
    assert_eq!(bus.access_count("pl011"), Some(1));
}

#[test]
fn tx_line_is_logged_at_debug() {
    let dir = std::env::temp_dir().join(format!("ternvale-uart-{}-tx", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let mut config = ternvale_log::LogConfig::new("uart", dir.clone());
    config.level = "trace".to_string();
    let guard = ternvale_log::init(config).expect("log");
    let (mut uart, _) = uart(Vec::new());
    for byte in b"Hi\n" {
        uart.write(0, 1, u64::from(*byte));
    }
    uart.write(0xfe0, 4, 1);
    drop(uart);
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    assert!(text.contains("uart tx"), "{text}");
    assert!(text.contains("line=Hi"), "{text}");
    assert!(text.contains("reg=\"UARTDR\""), "{text}");
    assert!(
        text.contains("ignored write to read-only pl011 register"),
        "{text}"
    );
    std::fs::remove_dir_all(&dir).expect("remove log dir");
}
