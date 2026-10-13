use std::cell::RefCell;

use ternvale_tpm::regs::{
    CTRL_REQ, CTRL_REQ_CMD_READY, CTRL_REQ_GO_IDLE, CTRL_START, CTRL_STS, CTRL_STS_IDLE,
    INTF_ID_HIGH, INTF_ID_LO, INTF_ID_LOW, LOC_CTRL, LOC_CTRL_RELINQUISH, LOC_CTRL_REQUEST_ACCESS,
    LOC_STATE, LOC_STATE_REG_VALID, LOC_STS, LOC_STS_GRANTED, START_INVOKE,
};
use ternvale_tpm::{CRB_WINDOW_SIZE, DATA_BUFFER, RC_COMMAND_CODE, ST_NO_SESSIONS};

use super::{device, register, TPM_DEVICE_NAME};
use crate::esr::ExitEvent;
use crate::mmio::{GuestRegs, MmioBus, MmioError};
use crate::platform::{Layout, TPM_CRB_BASE, TPM_CRB_SIZE};

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

/// A guest reaching the CRB only through MMIO exits, as Linux `tpm_crb` would.
struct Guest {
    bus: MmioBus,
    regs: Regs,
}

impl Guest {
    fn new() -> Self {
        let mut bus = MmioBus::new();
        register(&mut bus).expect("register");
        Self {
            bus,
            regs: Regs(RefCell::new([0; 31])),
        }
    }

    fn read(&self, offset: u64, size: u8) -> u64 {
        self.bus
            .dispatch(
                &self.regs,
                ExitEvent::Mmio {
                    gpa: TPM_CRB_BASE + offset,
                    size,
                    write: false,
                    reg: 3,
                },
            )
            .expect("read");
        self.regs.0.borrow()[3]
    }

    fn write(&self, offset: u64, size: u8, value: u64) {
        self.regs.0.borrow_mut()[4] = value;
        self.bus
            .dispatch(
                &self.regs,
                ExitEvent::Mmio {
                    gpa: TPM_CRB_BASE + offset,
                    size,
                    write: true,
                    reg: 4,
                },
            )
            .expect("write");
    }
}

#[test]
fn device_sits_in_the_tpm_crb_region() {
    let (base, size, crb) = device().expect("tpm device");
    assert_eq!(base, TPM_CRB_BASE);
    assert_eq!(size, CRB_WINDOW_SIZE);
    assert_eq!(crb.name(), TPM_DEVICE_NAME);
    let layout = Layout::virt(256 << 20);
    let region = layout
        .regions()
        .iter()
        .find(|r| r.name == TPM_DEVICE_NAME)
        .expect("tpm-crb region");
    assert_eq!((region.base, region.size), (TPM_CRB_BASE, TPM_CRB_SIZE));
}

#[test]
fn linux_command_sequence_through_the_bus_gets_a_stub_response() {
    let guest = Guest::new();
    assert_eq!(
        guest.read(INTF_ID_LO, 4),
        u64::from(INTF_ID_LOW),
        "interface id low"
    );
    assert_eq!(guest.read(INTF_ID_LO + 4, 4), u64::from(INTF_ID_HIGH));
    assert_eq!(
        guest.read(LOC_STATE, 4) & u64::from(LOC_STATE_REG_VALID),
        u64::from(LOC_STATE_REG_VALID)
    );

    guest.write(LOC_CTRL, 4, u64::from(LOC_CTRL_REQUEST_ACCESS));
    assert_eq!(
        guest.read(LOC_STS, 4) & u64::from(LOC_STS_GRANTED),
        u64::from(LOC_STS_GRANTED)
    );
    guest.write(CTRL_REQ, 4, u64::from(CTRL_REQ_CMD_READY));
    assert_eq!(guest.read(CTRL_STS, 4) & u64::from(CTRL_STS_IDLE), 0);

    // TPM2_GetRandom(8): 0x8001, size 12, TPM_CC_GetRandom 0x17b, bytesRequested 8.
    let command: [u8; 12] = [0x80, 0x01, 0, 0, 0, 12, 0, 0, 0x01, 0x7b, 0, 8];
    for (i, chunk) in command.chunks(4).enumerate() {
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        guest.write(DATA_BUFFER + 4 * i as u64, 4, u64::from(word));
    }
    guest.write(CTRL_START, 4, u64::from(START_INVOKE));
    assert_eq!(guest.read(CTRL_START, 4), 0, "start clears on completion");

    let header = guest.read(DATA_BUFFER, 8).to_le_bytes();
    let tail = guest.read(DATA_BUFFER + 8, 2) as u16;
    assert_eq!(u16::from_be_bytes([header[0], header[1]]), ST_NO_SESSIONS);
    assert_eq!(
        u32::from_be_bytes([header[2], header[3], header[4], header[5]]),
        10
    );
    let code = u32::from_be_bytes([header[6], header[7], (tail & 0xff) as u8, (tail >> 8) as u8]);
    assert_eq!(code, RC_COMMAND_CODE);

    guest.write(CTRL_REQ, 4, u64::from(CTRL_REQ_GO_IDLE));
    assert_eq!(
        guest.read(CTRL_STS, 4) & u64::from(CTRL_STS_IDLE),
        u64::from(CTRL_STS_IDLE)
    );
    guest.write(LOC_CTRL, 4, u64::from(LOC_CTRL_RELINQUISH));
    assert_eq!(guest.read(LOC_STS, 4) & u64::from(LOC_STS_GRANTED), 0);
}

#[test]
fn registering_twice_is_an_overlap() {
    let mut bus = MmioBus::new();
    register(&mut bus).expect("first");
    let error = register(&mut bus).unwrap_err();
    assert!(error.to_string().contains("overlaps"), "{error}");
}

#[test]
fn reserved_tail_of_the_region_is_unmapped() {
    let guest = Guest::new();
    assert_eq!(guest.read(CRB_WINDOW_SIZE, 4), 0);
    guest.write(CRB_WINDOW_SIZE, 4, 0xdead_beef);
    assert_eq!(guest.read(TPM_CRB_SIZE - 4, 4), 0);
}
