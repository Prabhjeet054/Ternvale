//! CRB register map.
//!
//! TCG PC Client Platform TPM Profile (PTP) Specification for TPM 2.0,
//! Version 1.05 Revision 14, §6.5.3 "CRB Interface" (the locality and control
//! area register tables). TODO(verify): the exact section and table numbers in
//! the revision you check against; the offsets and bits below match QEMU's
//! `hw/tpm/tpm_crb.c` and Linux's `drivers/char/tpm/tpm_crb.c`.
//!
//! Each locality `x` (0..=4) has a 4 KiB page at `base + x * 0x1000`:
//!
//! | Offset | Register | Width |
//! | --- | --- | --- |
//! | `0x00` | `TPM_LOC_STATE_x` | 32 |
//! | `0x08` | `TPM_LOC_CTRL_x` | 32 |
//! | `0x0c` | `TPM_LOC_STS_x` | 32 |
//! | `0x30` | `TPM_CRB_INTF_ID_x` | 64 |
//! | `0x38` | `TPM_CRB_CTRL_EXT_x` | 64 |
//! | `0x40` | `TPM_CRB_CTRL_REQ_x` (start of the control area) | 32 |
//! | `0x44` | `TPM_CRB_CTRL_STS_x` | 32 |
//! | `0x48` | `TPM_CRB_CTRL_CANCEL_x` | 32 |
//! | `0x4c` | `TPM_CRB_CTRL_START_x` | 32 |
//! | `0x50` | `TPM_CRB_INT_ENABLE_x` | 32 |
//! | `0x54` | `TPM_CRB_INT_STS_x` | 32 |
//! | `0x58` | `TPM_CRB_CTRL_CMD_SIZE_x` | 32 |
//! | `0x5c` | `TPM_CRB_CTRL_CMD_LADDR_x` | 32 |
//! | `0x60` | `TPM_CRB_CTRL_CMD_HADDR_x` | 32 |
//! | `0x64` | `TPM_CRB_CTRL_RSP_SIZE_x` | 32 |
//! | `0x68` | `TPM_CRB_CTRL_RSP_ADDR_x` | 64 |
//! | `0x80` | `TPM_CRB_DATA_BUFFER_x` (to the end of the page) | — |

/// Localities 0..=4.
pub const LOCALITIES: u8 = 5;
/// Bytes per locality page.
pub const LOCALITY_STRIDE: u64 = 0x1000;
/// The whole CRB window: one page per locality.
pub const CRB_WINDOW_SIZE: u64 = LOCALITY_STRIDE * LOCALITIES as u64;

/// `TPM_LOC_STATE_x`.
pub const LOC_STATE: u64 = 0x00;
/// `TPM_LOC_CTRL_x`.
pub const LOC_CTRL: u64 = 0x08;
/// `TPM_LOC_STS_x`.
pub const LOC_STS: u64 = 0x0c;
/// `TPM_CRB_INTF_ID_x`, low half.
pub const INTF_ID_LO: u64 = 0x30;
/// `TPM_CRB_INTF_ID_x`, high half.
pub const INTF_ID_HI: u64 = 0x34;
/// `TPM_CRB_CTRL_EXT_x`, low half.
pub const CTRL_EXT_LO: u64 = 0x38;
/// `TPM_CRB_CTRL_EXT_x`, high half.
pub const CTRL_EXT_HI: u64 = 0x3c;
/// `TPM_CRB_CTRL_REQ_x`: the first control area register.
pub const CTRL_REQ: u64 = 0x40;
/// `TPM_CRB_CTRL_STS_x`.
pub const CTRL_STS: u64 = 0x44;
/// `TPM_CRB_CTRL_CANCEL_x`.
pub const CTRL_CANCEL: u64 = 0x48;
/// `TPM_CRB_CTRL_START_x`.
pub const CTRL_START: u64 = 0x4c;
/// `TPM_CRB_INT_ENABLE_x`.
pub const INT_ENABLE: u64 = 0x50;
/// `TPM_CRB_INT_STS_x`.
pub const INT_STS: u64 = 0x54;
/// `TPM_CRB_CTRL_CMD_SIZE_x`.
pub const CMD_SIZE: u64 = 0x58;
/// `TPM_CRB_CTRL_CMD_LADDR_x`.
pub const CMD_LADDR: u64 = 0x5c;
/// `TPM_CRB_CTRL_CMD_HADDR_x`.
pub const CMD_HADDR: u64 = 0x60;
/// `TPM_CRB_CTRL_RSP_SIZE_x`.
pub const RSP_SIZE: u64 = 0x64;
/// `TPM_CRB_CTRL_RSP_ADDR_x`, low half.
pub const RSP_ADDR_LO: u64 = 0x68;
/// `TPM_CRB_CTRL_RSP_ADDR_x`, high half.
pub const RSP_ADDR_HI: u64 = 0x6c;
/// `TPM_CRB_DATA_BUFFER_x`.
pub const DATA_BUFFER: u64 = 0x80;
/// Command and response buffer size: the rest of the page (QEMU's
/// `CRB_CTRL_CMD_SIZE`).
pub const DATA_BUFFER_SIZE: usize = (LOCALITY_STRIDE - DATA_BUFFER) as usize;

/// Offset of the CRB control area from the window base. The ACPI TPM2 table's
/// "Address of CRB Control Area" is `base + CONTROL_AREA_OFFSET`, and Linux
/// requires it to sit right after the 0x40-byte locality header.
pub const CONTROL_AREA_OFFSET: u64 = CTRL_REQ;

/// `TPM_LOC_STATE_x.tpmEstablished`.
pub const LOC_STATE_ESTABLISHED: u32 = 1 << 0;
/// `TPM_LOC_STATE_x.locAssigned`.
pub const LOC_STATE_ASSIGNED: u32 = 1 << 1;
/// `TPM_LOC_STATE_x.activeLocality` (bits 4:2).
pub const LOC_STATE_ACTIVE_SHIFT: u32 = 2;
/// `TPM_LOC_STATE_x.tpmRegValidSts`.
pub const LOC_STATE_REG_VALID: u32 = 1 << 7;

/// `TPM_LOC_CTRL_x.requestAccess`.
pub const LOC_CTRL_REQUEST_ACCESS: u32 = 1 << 0;
/// `TPM_LOC_CTRL_x.Relinquish`.
pub const LOC_CTRL_RELINQUISH: u32 = 1 << 1;
/// `TPM_LOC_CTRL_x.Seize`.
pub const LOC_CTRL_SEIZE: u32 = 1 << 2;
/// `TPM_LOC_CTRL_x.resetEstablishmentBit`.
pub const LOC_CTRL_RESET_ESTABLISHMENT: u32 = 1 << 3;

/// `TPM_LOC_STS_x.Granted`.
pub const LOC_STS_GRANTED: u32 = 1 << 0;
/// `TPM_LOC_STS_x.beenSeized`.
pub const LOC_STS_BEEN_SEIZED: u32 = 1 << 1;

/// `TPM_CRB_CTRL_REQ_x.cmdReady`.
pub const CTRL_REQ_CMD_READY: u32 = 1 << 0;
/// `TPM_CRB_CTRL_REQ_x.goIdle`.
pub const CTRL_REQ_GO_IDLE: u32 = 1 << 1;

/// `TPM_CRB_CTRL_STS_x.tpmSts`: fatal error.
pub const CTRL_STS_FATAL: u32 = 1 << 0;
/// `TPM_CRB_CTRL_STS_x.tpmIdle`.
pub const CTRL_STS_IDLE: u32 = 1 << 1;

/// `TPM_CRB_CTRL_START_x` / `TPM_CRB_CTRL_CANCEL_x` bit 0.
pub const START_INVOKE: u32 = 1 << 0;

/// `TPM_CRB_INTF_ID_x` fields (low 32 bits).
pub mod intf_id {
    /// InterfaceType (3:0) = 1: CRB interface active.
    pub const TYPE_CRB_ACTIVE: u32 = 1;
    /// InterfaceVersion (7:4) = 1: CRB.
    pub const VERSION_CRB: u32 = 1 << 4;
    /// CapLocality (8): localities 0..=4 are supported.
    pub const CAP_LOCALITY: u32 = 1 << 8;
    /// CapDataXferSizeSupport (12:11) = 3: 64-byte transfers.
    pub const CAP_XFER_64: u32 = 3 << 11;
    /// CapCRB (14).
    pub const CAP_CRB: u32 = 1 << 14;
    /// InterfaceSelector (18:17) = 1: CRB selected.
    pub const SELECTOR_CRB: u32 = 1 << 17;
    /// Vendor ID (high word bits 15:0). QEMU's `tpm-crb` reports IBM's PCI
    /// vendor ID; Ternvale reports the same so guests see a known emulated
    /// CRB. TODO(verify): whether Windows keys any behavior off VID/DID.
    pub const VID: u32 = 0x1014;
    /// Device ID (high word bits 31:16).
    pub const DID: u32 = 0;
}

/// Low half of `TPM_CRB_INTF_ID_x`.
pub const INTF_ID_LOW: u32 = intf_id::TYPE_CRB_ACTIVE
    | intf_id::VERSION_CRB
    | intf_id::CAP_LOCALITY
    | intf_id::CAP_XFER_64
    | intf_id::CAP_CRB
    | intf_id::SELECTOR_CRB;
/// High half of `TPM_CRB_INTF_ID_x`.
pub const INTF_ID_HIGH: u32 = intf_id::VID | (intf_id::DID << 16);
