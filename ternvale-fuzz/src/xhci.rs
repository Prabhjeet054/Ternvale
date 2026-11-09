//! xHCI command/transfer TRB processing through MMIO.

use std::sync::{Arc, Mutex};

use arbitrary::{Arbitrary, Unstructured};
use ternvale_usb::{GuestMemoryAccess, MemError, Trb, Xhci, TRB_SIZE};

use crate::quiet;

const RING: u64 = 0x4000_1000;
const EVENT: u64 = 0x4000_2000;
const RING_BYTES: usize = 0x1000;
const EVENT_BYTES: usize = 0x1000;
const OTHER_BASE: usize = RING_BYTES + EVENT_BYTES;
const OTHER_BYTES: usize = 0x4000;
const MAX: usize = OTHER_BASE + OTHER_BYTES;
/// Operational `USBCMD` (xHCI §5.4.1).
const USBCMD: u64 = 0x20;
/// Operational `CRCR` (xHCI §5.4.5).
const CRCR: u64 = 0x38;
/// `USBCMD` run/stop.
const CMD_RUN: u32 = 1;
/// Runtime ERSTSZ for interrupter 0.
const ERSTSZ: u64 = 0x2008;
/// Runtime ERSTBA for interrupter 0.
const ERSTBA: u64 = 0x2010;
/// Host controller doorbell (slot 0).
const DOORBELL0: u64 = 0x4000;
/// `TRB_TYPE_ENABLE_SLOT` in bits 15:10, cycle bit set.
const ENABLE_SLOT_CYCLE: u32 = (9 << 10) | 1;

#[derive(Debug, Arbitrary)]
struct TrbInput {
    param: u64,
    status: u32,
    control: u32,
}

/// Translate a guest ring GPA into an index in the bounded host buffer.
fn slot(gpa: u64) -> usize {
    if (RING..RING + RING_BYTES as u64).contains(&gpa) {
        (gpa - RING) as usize
    } else if (EVENT..EVENT + EVENT_BYTES as u64).contains(&gpa) {
        RING_BYTES + (gpa - EVENT) as usize
    } else {
        OTHER_BASE + (gpa as usize % OTHER_BYTES)
    }
}

/// Bounded GPA map for rings (guest-hostile reads never panic).
struct FlatMem {
    bytes: Vec<u8>,
}

impl GuestMemoryAccess for FlatMem {
    fn read_bytes(&self, gpa: u64, buf: &mut [u8]) -> Result<(), MemError> {
        for (i, dest) in buf.iter_mut().enumerate() {
            *dest = self
                .bytes
                .get(slot(gpa.saturating_add(i as u64)))
                .copied()
                .unwrap_or(0);
        }
        Ok(())
    }

    fn write_bytes(&mut self, gpa: u64, data: &[u8]) -> Result<(), MemError> {
        for (i, byte) in data.iter().enumerate() {
            if let Some(dest) = self.bytes.get_mut(slot(gpa.saturating_add(i as u64))) {
                *dest = *byte;
            }
        }
        Ok(())
    }
}

struct Shared(Arc<Mutex<FlatMem>>);

impl GuestMemoryAccess for Shared {
    fn read_bytes(&self, gpa: u64, buf: &mut [u8]) -> Result<(), MemError> {
        self.0
            .lock()
            .map_err(|_| MemError::Read {
                gpa,
                message: "poison".into(),
            })?
            .read_bytes(gpa, buf)
    }

    fn write_bytes(&mut self, gpa: u64, data: &[u8]) -> Result<(), MemError> {
        self.0
            .lock()
            .map_err(|_| MemError::Write {
                gpa,
                message: "poison".into(),
            })?
            .write_bytes(gpa, data)
    }
}

fn put_trb(bytes: &mut [u8], index: usize, trb: Trb) {
    let off = index * TRB_SIZE;
    if off + TRB_SIZE <= RING_BYTES && off + TRB_SIZE <= bytes.len() {
        bytes[off..off + TRB_SIZE].copy_from_slice(&trb.to_bytes());
    }
}

/// Place TRBs in fake RAM, run the controller, doorbell the command ring.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let mut bytes = vec![0u8; MAX];
    // Default command: ENABLE_SLOT with cycle=1 so a doorbell walks a real TRB.
    put_trb(
        &mut bytes,
        0,
        Trb {
            param: 0,
            status: 0,
            control: ENABLE_SLOT_CYCLE,
        },
    );
    if let Ok(list) = Vec::<TrbInput>::arbitrary(&mut Unstructured::new(data)) {
        for (i, item) in list.iter().take(16).enumerate() {
            put_trb(
                &mut bytes,
                i,
                Trb {
                    param: item.param,
                    status: item.status,
                    control: item.control,
                },
            );
        }
    } else {
        let n = data.len().min(16 * TRB_SIZE);
        bytes[..n].copy_from_slice(&data[..n]);
        let _ = Trb::from_bytes(data);
    }
    let raw = Arc::new(Mutex::new(FlatMem { bytes }));
    let dynmem: Arc<Mutex<dyn GuestMemoryAccess + Send>> =
        Arc::new(Mutex::new(Shared(Arc::clone(&raw))));
    let Ok(mut xhci) = Xhci::new(0x091c_0000, dynmem) else {
        return;
    };
    xhci.write(USBCMD, 4, u64::from(CMD_RUN));
    xhci.write(CRCR, 8, RING | 1);
    xhci.write(ERSTSZ, 4, 32);
    xhci.write(ERSTBA, 8, EVENT);
    xhci.write(DOORBELL0, 4, 0);
    for chunk in data.chunks(6).take(16) {
        if chunk.len() < 4 {
            break;
        }
        let offset = u16::from_le_bytes([chunk[0], chunk[1]]) as u64 % 0x5000;
        let size = match chunk[2] % 3 {
            0 => 1,
            1 => 4,
            _ => 8,
        };
        let value = u32::from_le_bytes([
            chunk.get(2).copied().unwrap_or(0),
            chunk.get(3).copied().unwrap_or(0),
            chunk.get(4).copied().unwrap_or(0),
            chunk.get(5).copied().unwrap_or(0),
        ]);
        if chunk[0] & 1 == 0 {
            let _ = xhci.read(offset, size);
        } else {
            xhci.write(offset, size, u64::from(value));
        }
    }
}
