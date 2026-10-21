//! xHCI MMIO and command-ring tests without a hypervisor.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::guest_mem::{GuestMemoryAccess, MemError};
use crate::hid::device_descriptor;
use crate::regs::{
    doorbell, CAPLENGTH, CMD_RUN, CRCR, EXT_CAP_BASE, HCCPARAMS1, HCCPARAMS1_VALUE, HCSPARAMS1,
    NUM_PORTS, USBCMD,
};
use crate::trb::{
    Trb, TRB_TYPE_DATA_STAGE, TRB_TYPE_DISABLE_SLOT, TRB_TYPE_ENABLE_SLOT, TRB_TYPE_SETUP_STAGE,
    TRB_TYPE_STATUS_STAGE,
};
use crate::xhci::{attach_shared, Xhci};

#[derive(Default)]
struct TestMem {
    bytes: HashMap<u64, u8>,
}

impl TestMem {
    fn write_region(&mut self, gpa: u64, data: &[u8]) {
        for (index, byte) in data.iter().enumerate() {
            self.bytes.insert(gpa + index as u64, *byte);
        }
    }
}

impl GuestMemoryAccess for TestMem {
    fn read_bytes(&self, gpa: u64, buf: &mut [u8]) -> Result<(), MemError> {
        for (index, slot) in buf.iter_mut().enumerate() {
            *slot = self.bytes.get(&(gpa + index as u64)).copied().unwrap_or(0);
        }
        Ok(())
    }

    fn write_bytes(&mut self, gpa: u64, data: &[u8]) -> Result<(), MemError> {
        self.write_region(gpa, data);
        Ok(())
    }
}

struct DynMem(Arc<Mutex<TestMem>>);

impl GuestMemoryAccess for DynMem {
    fn read_bytes(&self, gpa: u64, buf: &mut [u8]) -> Result<(), MemError> {
        self.0
            .lock()
            .map_err(|_| MemError::Read {
                gpa,
                message: "lock poisoned".into(),
            })?
            .read_bytes(gpa, buf)
    }

    fn write_bytes(&mut self, gpa: u64, data: &[u8]) -> Result<(), MemError> {
        self.0
            .lock()
            .map_err(|_| MemError::Write {
                gpa,
                message: "lock poisoned".into(),
            })?
            .write_bytes(gpa, data)
    }
}

fn mem_pair() -> (
    Arc<Mutex<TestMem>>,
    Arc<Mutex<dyn GuestMemoryAccess + Send>>,
) {
    let raw = Arc::new(Mutex::new(TestMem::default()));
    let dynmem = Arc::new(Mutex::new(DynMem(Arc::clone(&raw))));
    (raw, dynmem)
}

fn configure_rings(xhci: &mut Xhci, cmd_ring: u64, event_ring: u64) {
    xhci.write(USBCMD, 4, u64::from(CMD_RUN));
    xhci.write(CRCR, 8, cmd_ring | 1);
    xhci.write(crate::regs::rt_erstsz(0), 4, 32);
    xhci.write(crate::regs::rt_erstba(0), 8, event_ring);
}

fn post_command_trb(mem: &mut TestMem, ptr: u64, ty: u8, cycle: bool) {
    let trb = Trb {
        param: 0,
        status: 0,
        control: (ty as u32) << 10 | u32::from(cycle),
    };
    mem.write_region(ptr, &trb.to_bytes());
}

#[test]
fn capability_registers_describe_ports_and_slots() {
    let (_, dynmem) = mem_pair();
    let xhci = Xhci::new(0x091c_0000, dynmem).expect("xhci");
    assert_eq!(xhci.read(0, 4), u32::from(CAPLENGTH) as u64);
    let hcs = xhci.read(HCSPARAMS1, 4) as u32;
    assert_eq!(hcs >> 24, u32::from(NUM_PORTS));
    assert_eq!(xhci.read(HCCPARAMS1, 4) as u32, HCCPARAMS1_VALUE);
    assert_ne!(xhci.read(EXT_CAP_BASE, 4), 0);
}

#[test]
fn attach_exposes_usb_input() {
    let (_, dynmem) = mem_pair();
    let irq = Arc::new(|_: bool| {});
    let (_xhci, input) = attach_shared(0x091c_0000, dynmem, irq).expect("attach");
    let _ = input.stats();
}

#[test]
fn command_doorbell_enable_and_disable_slot() {
    let (raw, dynmem) = mem_pair();
    let mut xhci = Xhci::new(0x091c_0000, dynmem).expect("xhci");
    let cmd_ring = 0x4000_1000u64;
    let event_ring = 0x4000_2000u64;
    configure_rings(&mut xhci, cmd_ring, event_ring);
    {
        let mut inner = raw.lock().expect("mem");
        post_command_trb(&mut inner, cmd_ring, TRB_TYPE_ENABLE_SLOT, true);
        let disable = Trb {
            param: 0,
            status: 0,
            control: (TRB_TYPE_DISABLE_SLOT as u32) << 10 | (1 << 24) | 1,
        };
        inner.write_region(cmd_ring + 16, &disable.to_bytes());
    }
    xhci.write(doorbell(0), 4, 0);
    let inner = raw.lock().expect("mem");
    let ev0 = crate::mem::read_trb(&*inner, event_ring).expect("event");
    assert_eq!(ev0.trb_type(), 33);
}

#[test]
fn get_descriptor_control_transfer_posts_transfer_events() {
    let (raw, dynmem) = mem_pair();
    let mut xhci = Xhci::new(0x091c_0000, dynmem).expect("xhci");
    let event_ring = 0x4000_2000u64;
    configure_rings(&mut xhci, 0x4000_1000, event_ring);
    let setup_gpa = 0x4000_3000u64;
    let data_gpa = 0x4000_3100u64;
    let dev = device_descriptor(0x0627, 0x0001);
    let setup = [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, dev.len() as u8, 0];
    let xfer_ring = 0x4000_4000u64;
    {
        let mut inner = raw.lock().expect("mem");
        inner.write_region(setup_gpa, &setup);
        let trbs = [
            Trb {
                param: setup_gpa,
                status: 8,
                control: (TRB_TYPE_SETUP_STAGE as u32) << 10 | 1,
            },
            Trb {
                param: data_gpa,
                status: dev.len() as u32,
                control: (TRB_TYPE_DATA_STAGE as u32) << 10 | (1 << 2) | 1,
            },
            Trb {
                param: 0,
                status: 0,
                control: (TRB_TYPE_STATUS_STAGE as u32) << 10 | (1 << 3) | 1,
            },
        ];
        for (index, trb) in trbs.iter().enumerate() {
            inner.write_region(xfer_ring + index as u64 * 16, &trb.to_bytes());
        }
        post_command_trb(&mut inner, 0x4000_1000, TRB_TYPE_ENABLE_SLOT, true);
    }
    xhci.write(doorbell(0), 4, 0);
    xhci.seed_ep0_ring(1, xfer_ring);
    xhci.write(doorbell(1), 4, 0);
    let inner = raw.lock().expect("mem");
    let mut saw_cmd = false;
    let mut saw_xfer = false;
    for index in 0..8u64 {
        let ev = crate::mem::read_trb(&*inner, event_ring + index * 16).expect("event");
        match ev.trb_type() {
            33 => saw_cmd = true,
            32 => saw_xfer = true,
            _ => {}
        }
    }
    assert!(saw_cmd, "command completion event");
    assert!(saw_xfer, "transfer completion event for GET_DESCRIPTOR");
    let data = crate::mem::read_bytes(&*inner, data_gpa, dev.len()).expect("data");
    assert_eq!(
        &data[..dev.len()],
        &dev,
        "GET_DESCRIPTOR data stage payload"
    );
}
