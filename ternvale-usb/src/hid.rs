//! Virtual USB HID boot keyboard and mouse (no hardware passthrough).

/// Which root port carries the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidKind {
    /// Boot keyboard (port 1).
    Keyboard,
    /// Boot mouse (port 2).
    Mouse,
}

impl HidKind {
    /// Root hub port index (1-based).
    pub fn port(self) -> u8 {
        match self {
            Self::Keyboard => 1,
            Self::Mouse => 2,
        }
    }

    /// USB device address assigned with the slot (simplified).
    pub fn default_address(self) -> u8 {
        match self {
            Self::Keyboard => 1,
            Self::Mouse => 2,
        }
    }
}

/// Boot keyboard report (8 bytes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BootKeyboardReport {
    /// Modifier byte.
    pub modifiers: u8,
    /// Reserved.
    pub reserved: u8,
    /// Up to six key codes.
    pub keys: [u8; 6],
}

impl BootKeyboardReport {
    /// Wire format for interrupt IN.
    pub fn to_bytes(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0] = self.modifiers;
        out[2..8].copy_from_slice(&self.keys);
        out
    }
}

/// Boot mouse report (3 bytes: buttons, dx, dy).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BootMouseReport {
    /// Button bitmask (bit0 left, bit1 right, bit2 middle).
    pub buttons: u8,
    /// Relative X.
    pub dx: i8,
    /// Relative Y.
    pub dy: i8,
}

impl BootMouseReport {
    /// Wire format for interrupt IN.
    pub fn to_bytes(self) -> [u8; 3] {
        [self.buttons, self.dx as u8, self.dy as u8]
    }
}

/// Standard USB device descriptor (18 bytes) for a HID boot device.
pub fn device_descriptor(vid: u16, pid: u16) -> [u8; 18] {
    let mut d = [0u8; 18];
    d[0] = 18;
    d[1] = 1; // DEVICE
    d[2] = 0x00;
    d[3] = 0x02; // bcdUSB 2.0
    d[4] = 0;
    d[5] = 0;
    d[6] = 0;
    d[7] = 8; // max packet ep0
    d[8..10].copy_from_slice(&vid.to_le_bytes());
    d[10..12].copy_from_slice(&pid.to_le_bytes());
    d[12] = 0;
    d[13] = 0;
    d[14] = 0;
    d[15] = 1; // one configuration
    d[16] = 0;
    d[17] = 0;
    d
}

/// Minimal configuration + interface + HID + endpoint descriptors.
pub fn config_bundle(kind: HidKind, max_packet: u8) -> Vec<u8> {
    let report_desc = match kind {
        HidKind::Keyboard => BOOT_KEYBOARD_REPORT_DESC,
        HidKind::Mouse => BOOT_MOUSE_REPORT_DESC,
    };
    let total = 9 + 9 + 9 + 7 + 7 + report_desc.len();
    let mut out = Vec::with_capacity(total);
    // Configuration descriptor
    out.extend_from_slice(&[9, 2, (total as u8), 0, 1, 1, 0, 0x80, 50]);
    // Interface (HID boot)
    out.extend_from_slice(&[
        9,
        4,
        0,
        0,
        1,
        0x03,
        match kind {
            HidKind::Keyboard => 0x01,
            HidKind::Mouse => 0x02,
        },
        0,
        0,
    ]);
    // HID descriptor
    out.extend_from_slice(&[
        9,
        0x21,
        0x11,
        0x01,
        0,
        1,
        0x22,
        (report_desc.len() as u8),
        0,
    ]);
    // Interrupt IN endpoint
    out.extend_from_slice(&[7, 5, 0x81, 0x03, max_packet, 0x0a, 0]);
    out.extend_from_slice(report_desc);
    out
}

/// Boot keyboard report descriptor.
pub const BOOT_KEYBOARD_REPORT_DESC: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x06, 0x75, 0x08,
    0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xc0,
];

/// Boot mouse report descriptor.
pub const BOOT_MOUSE_REPORT_DESC: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06,
    0xc0, 0xc0,
];
