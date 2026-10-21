//! xHCI MMIO register map (capability, operational, runtime, doorbells).
//!
//! Layout follows xHCI §5.3–5.5 and matches the offsets Linux `xhci.h` uses.
//! TODO(verify): exact capability field values against the revision you cite.

/// Guest-visible MMIO window size (cap + op + runtime + doorbells).
pub const MMIO_SIZE: u64 = 0x5000;

pub const CAPLENGTH: u8 = 0x20;
pub const HCIVERSION: u16 = 0x0100;

/// Root hub port count (virtual HID + passthrough ports).
pub const NUM_PORTS: u8 = 4;
/// Device slot contexts the controller offers (slot 0 unused by guests).
pub const MAX_SLOTS: u8 = 8;
/// One interrupter (event ring).
pub const MAX_INTRS: u8 = 1;

/// Capability: `HCSPARAMS1`.
pub const HCSPARAMS1: u64 = 0x04;
/// Capability: `HCSPARAMS2`.
pub const HCSPARAMS2: u64 = 0x08;
/// Capability: `HCCPARAMS1`.
pub const HCCPARAMS1: u64 = 0x10;
/// First extended capability register (Supported Protocol), byte offset from cap base (xHCI §7.2).
pub const EXT_CAP_BASE: u64 = 0x20;
/// `HCCPARAMS1` value: 64-bit addressing + extended-cap pointer at [`EXT_CAP_BASE`].
pub const HCCPARAMS1_VALUE: u32 = 1 | ((EXT_CAP_BASE as u32) << 16);
/// Capability: doorbell array offset.
pub const DBOFF: u32 = 0x4000;
/// Capability: runtime register space offset.
pub const RTSOFF: u32 = 0x2000;

/// Operational `USBCMD` (immediately after the capability registers).
pub const USBCMD: u64 = 0x20;
/// Operational `USBSTS`.
pub const USBSTS: u64 = USBCMD + 4;
/// Operational `PAGESIZE`.
pub const PAGESIZE: u64 = USBCMD + 8;
/// Operational `DNCTRL`.
pub const DNCTRL: u64 = USBCMD + 0x14;
/// Operational `CRCR` (64-bit), low dword.
pub const CRCR: u64 = USBCMD + 0x18;
/// Operational `CRCR`, high dword.
pub const CRCR_HI: u64 = CRCR + 4;
/// Operational `DCBAAP` (64-bit), low dword.
pub const DCBAAP: u64 = USBCMD + 0x30;
/// Operational `DCBAAP`, high dword.
pub const DCBAAP_HI: u64 = DCBAAP + 4;
/// Operational `CONFIG` (max device slots enabled).
pub const CONFIG: u64 = USBCMD + 0x38;
/// First `PORTSC` register.
pub const PORTSC_BASE: u64 = USBCMD + 0x400;

/// Runtime: interrupter 0 `IMAN`.
pub fn rt_iman(intr: u8) -> u64 {
    u64::from(RTSOFF) + u64::from(intr) * 0x20
}
/// Runtime: `ERSTSZ`.
pub fn rt_erstsz(intr: u8) -> u64 {
    rt_iman(intr) + 0x08
}
/// Runtime: `ERSTBA`.
pub fn rt_erstba(intr: u8) -> u64 {
    rt_iman(intr) + 0x10
}
/// Runtime: `ERDP`.
pub fn rt_erdp(intr: u8) -> u64 {
    rt_iman(intr) + 0x18
}

/// Doorbell register for `slot` (slot 0 = host / command ring).
pub fn doorbell(slot: u8) -> u64 {
    u64::from(DBOFF) + u64::from(slot) * 4
}

/// `USBCMD` run/stop.
pub const CMD_RUN: u32 = 1 << 0;
/// `USBCMD` host controller reset.
pub const CMD_HCRST: u32 = 1 << 1;
/// `USBSTS` host controller halted.
pub const STS_HALT: u32 = 1 << 0;
#[allow(dead_code)]
/// `USBSTS` host system error (unused; kept clear).
pub const STS_HSE: u32 = 1 << 2;
/// `USBSTS` event interrupt.
pub const STS_EINT: u32 = 1 << 3;
/// `USBCMD` interrupter enable.
pub const CMD_INTE: u32 = 1 << 2;
/// Runtime `IMAN` interrupt pending.
pub const IMAN_IP: u32 = 1 << 0;
/// Runtime `IMAN` interrupt enable.
pub const IMAN_IE: u32 = 1 << 1;
/// `PORTSC` current connect status.
pub const PORT_CCS: u32 = 1 << 0;
/// `PORTSC` port enabled.
pub const PORT_PED: u32 = 1 << 1;
/// `PORTSC` port reset (RW1S).
pub const PORT_PR: u32 = 1 << 4;

/// Supported Protocol extended capability dword (xHCI §7.2).
#[must_use]
pub fn ext_cap_word(offset: u64) -> u32 {
    let port_map = u32::from(NUM_PORTS) << 8 | 1;
    match offset {
        EXT_CAP_BASE => 0x0002, // USB 2.0 only (single roothub; xHCI §7.2)
        0x24 => 0x02,           // major revision 2
        0x28 => port_map,
        _ => 0,
    }
}
