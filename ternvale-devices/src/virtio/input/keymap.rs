//! macOS virtual key codes (`kVK_*`, Carbon `HIToolbox/Events.h`) to Linux
//! `KEY_*` codes (`include/uapi/linux/input-event-codes.h`), by physical key
//! position, so the guest's own keyboard layout decides what a key types.
//!
//! The Mac `Help` key (where PC keyboards have Insert) maps to `KEY_INSERT`,
//! and `fn` is not forwarded. JIS Eisu/Kana have no mapping yet: TODO(verify)
//! whether `KEY_HANJA`/`KEY_HANGEUL` (QEMU's choice) suit Japanese guests.

/// `(kVK, KEY_*)` pairs.
const MAP: &[(u16, u16)] = &[
    (0x00, 30),  // A
    (0x01, 31),  // S
    (0x02, 32),  // D
    (0x03, 33),  // F
    (0x04, 35),  // H
    (0x05, 34),  // G
    (0x06, 44),  // Z
    (0x07, 45),  // X
    (0x08, 46),  // C
    (0x09, 47),  // V
    (0x0a, 86),  // ISO section -> KEY_102ND
    (0x0b, 48),  // B
    (0x0c, 16),  // Q
    (0x0d, 17),  // W
    (0x0e, 18),  // E
    (0x0f, 19),  // R
    (0x10, 21),  // Y
    (0x11, 20),  // T
    (0x12, 2),   // 1
    (0x13, 3),   // 2
    (0x14, 4),   // 3
    (0x15, 5),   // 4
    (0x16, 7),   // 6
    (0x17, 6),   // 5
    (0x18, 13),  // =
    (0x19, 10),  // 9
    (0x1a, 8),   // 7
    (0x1b, 12),  // -
    (0x1c, 9),   // 8
    (0x1d, 11),  // 0
    (0x1e, 27),  // ]
    (0x1f, 24),  // O
    (0x20, 22),  // U
    (0x21, 26),  // [
    (0x22, 23),  // I
    (0x23, 25),  // P
    (0x24, 28),  // Return -> KEY_ENTER
    (0x25, 38),  // L
    (0x26, 36),  // J
    (0x27, 40),  // ' -> KEY_APOSTROPHE
    (0x28, 37),  // K
    (0x29, 39),  // ;
    (0x2a, 43),  // backslash
    (0x2b, 51),  // ,
    (0x2c, 53),  // /
    (0x2d, 49),  // N
    (0x2e, 50),  // M
    (0x2f, 52),  // .
    (0x30, 15),  // Tab
    (0x31, 57),  // Space
    (0x32, 41),  // ` -> KEY_GRAVE
    (0x33, 14),  // Delete -> KEY_BACKSPACE
    (0x35, 1),   // Escape
    (0x36, 126), // Right Command -> KEY_RIGHTMETA
    (0x37, 125), // Command -> KEY_LEFTMETA
    (0x38, 42),  // Shift
    (0x39, 58),  // Caps Lock
    (0x3a, 56),  // Option -> KEY_LEFTALT
    (0x3b, 29),  // Control
    (0x3c, 54),  // Right Shift
    (0x3d, 100), // Right Option -> KEY_RIGHTALT
    (0x3e, 97),  // Right Control
    (0x40, 187), // F17
    (0x41, 83),  // keypad . -> KEY_KPDOT
    (0x43, 55),  // keypad * -> KEY_KPASTERISK
    (0x45, 78),  // keypad +
    (0x47, 69),  // keypad Clear -> KEY_NUMLOCK
    (0x48, 115), // Volume Up
    (0x49, 114), // Volume Down
    (0x4a, 113), // Mute
    (0x4b, 98),  // keypad /
    (0x4c, 96),  // keypad Enter
    (0x4e, 74),  // keypad -
    (0x4f, 188), // F18
    (0x50, 189), // F19
    (0x51, 117), // keypad =
    (0x52, 82),  // keypad 0
    (0x53, 79),  // keypad 1
    (0x54, 80),  // keypad 2
    (0x55, 81),  // keypad 3
    (0x56, 75),  // keypad 4
    (0x57, 76),  // keypad 5
    (0x58, 77),  // keypad 6
    (0x59, 71),  // keypad 7
    (0x5a, 190), // F20
    (0x5b, 72),  // keypad 8
    (0x5c, 73),  // keypad 9
    (0x5d, 124), // JIS Yen -> KEY_YEN
    (0x5e, 89),  // JIS Underscore -> KEY_RO
    (0x5f, 95),  // JIS keypad comma -> KEY_KPJPCOMMA
    (0x60, 63),  // F5
    (0x61, 64),  // F6
    (0x62, 65),  // F7
    (0x63, 61),  // F3
    (0x64, 66),  // F8
    (0x65, 67),  // F9
    (0x67, 87),  // F11
    (0x69, 183), // F13
    (0x6a, 186), // F16
    (0x6b, 184), // F14
    (0x6d, 68),  // F10
    (0x6e, 127), // Contextual Menu -> KEY_COMPOSE
    (0x6f, 88),  // F12
    (0x71, 185), // F15
    (0x72, 110), // Help -> KEY_INSERT
    (0x73, 102), // Home
    (0x74, 104), // Page Up
    (0x75, 111), // Forward Delete -> KEY_DELETE
    (0x76, 62),  // F4
    (0x77, 107), // End
    (0x78, 60),  // F2
    (0x79, 109), // Page Down
    (0x7a, 59),  // F1
    (0x7b, 105), // Left
    (0x7c, 106), // Right
    (0x7d, 108), // Down
    (0x7e, 103), // Up
];

/// The Linux key for macOS virtual key code `kvk`, if it has one.
pub fn linux_key(kvk: u16) -> Option<u16> {
    MAP.iter()
        .find(|(mac, _)| *mac == kvk)
        .map(|(_, linux)| *linux)
}

/// Every Linux key the map can produce (what the keyboard advertises).
pub fn linux_keys() -> impl Iterator<Item = u16> {
    MAP.iter().map(|(_, linux)| *linux)
}
