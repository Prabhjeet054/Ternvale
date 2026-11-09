//! TPM CRB register sequences (stub backend, no swtpm).

use arbitrary::{Arbitrary, Unstructured};
use ternvale_tpm::regs::{
    CTRL_REQ, CTRL_REQ_CMD_READY, CTRL_START, DATA_BUFFER, LOCALITY_STRIDE, LOC_CTRL,
    LOC_CTRL_REQUEST_ACCESS,
};
use ternvale_tpm::{StubBackend, TpmCrb};
use ternvale_vmm::TPM_CRB_BASE;

use crate::quiet;

#[derive(Debug, Arbitrary)]
enum Op {
    Read { offset: u16, size: u8 },
    Write { offset: u16, size: u8, value: u64 },
}

/// Claim locality 0, write a command buffer, START, then leftover MMIO.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Ok(mut crb) = TpmCrb::new(TPM_CRB_BASE, Box::new(StubBackend::new())) else {
        return;
    };
    crb.write(LOC_CTRL, 4, u64::from(LOC_CTRL_REQUEST_ACCESS));
    crb.write(CTRL_REQ, 4, u64::from(CTRL_REQ_CMD_READY));
    let cmd = data.iter().copied().take(64).collect::<Vec<_>>();
    for (i, byte) in cmd.iter().enumerate() {
        crb.write(DATA_BUFFER + i as u64, 1, u64::from(*byte));
    }
    crb.write(CTRL_START, 4, 1);
    let _ = crb.read(CTRL_START, 4);
    if let Ok(ops) = Vec::<Op>::arbitrary(&mut Unstructured::new(data)) {
        for op in ops.into_iter().take(32) {
            match op {
                Op::Read { offset, size } => {
                    let _ = crb.read(u64::from(offset) % (LOCALITY_STRIDE * 5), width(size));
                }
                Op::Write {
                    offset,
                    size,
                    value,
                } => {
                    crb.write(
                        u64::from(offset) % (LOCALITY_STRIDE * 5),
                        width(size),
                        value,
                    );
                }
            }
        }
        return;
    }
    for chunk in data.chunks(6).take(16) {
        if chunk.len() < 4 {
            break;
        }
        let offset = u16::from_le_bytes([chunk[0], chunk[1]]) as u64 % (LOCALITY_STRIDE * 5);
        let size = width(chunk[2]);
        let value = u32::from_le_bytes([
            chunk.get(2).copied().unwrap_or(0),
            chunk.get(3).copied().unwrap_or(0),
            chunk.get(4).copied().unwrap_or(0),
            chunk.get(5).copied().unwrap_or(0),
        ]);
        if chunk[0] & 1 == 0 {
            let _ = crb.read(offset, size);
        } else {
            crb.write(offset, size, u64::from(value));
        }
    }
}

fn width(raw: u8) -> u8 {
    match raw % 4 {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    }
}
