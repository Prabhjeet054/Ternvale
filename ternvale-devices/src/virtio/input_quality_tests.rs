//! Step 110 input analogs: AZERTY/Devanagari scancodes, key-up after a held
//! key (Cmd+Tab / focus loss), gamepad reports an `evtest` would see.

use super::keymap;
use super::tests::{Fx, WRITE};
use super::*;

/// Physical kVK → Linux KEY_*. The guest layout (AZERTY, IME) maps KEY_* to
/// glyphs; the host never sends Unicode.
#[test]
fn azerty_and_devanagari_use_physical_linux_keys() {
    // ANSI Q (kVK 0x0c) is KEY_Q. An AZERTY guest types 'a' from KEY_Q.
    assert_eq!(keymap::linux_key(0x0c), Some(16), "KEY_Q");
    // ANSI A (kVK 0x00) is KEY_A. AZERTY types 'q' from KEY_A.
    assert_eq!(keymap::linux_key(0x00), Some(30), "KEY_A");
    // Dead-key / accent position on ISO (section) is KEY_102ND, not a composed
    // character.
    assert_eq!(keymap::linux_key(0x0a), Some(86), "KEY_102ND");
    // Devanagari is composed by the guest IME from the same KEY_* stream.
    for kvk in [0x00u16, 0x0c, 0x0e, 0x11] {
        let code = keymap::linux_key(kvk).expect("letter");
        assert!(
            (2..=53).contains(&code),
            "{kvk:#x} -> {code} is not a Unicode code point"
        );
        assert!(InputKind::Keyboard.supports(EV_KEY, code));
    }
}

/// Held key then explicit release (viewer `releaseAll` on Cmd+Tab / resign).
#[test]
fn held_key_release_reaches_eventq() {
    let mut fx = Fx::new(InputKind::Keyboard);
    fx.post(0, 0, 8, WRITE);
    fx.kick(0);
    assert!(fx.sink.key(30, true), "A down");
    assert!(fx.sink.key(30, false), "A up (focus loss)");
    assert_eq!(fx.event(0, 0), InputEvent::key(30, true));
    assert_eq!(fx.event(0, 1), InputEvent::syn());
    assert_eq!(fx.event(0, 2), InputEvent::key(30, false));
    assert_eq!(fx.event(0, 3), InputEvent::syn());
    assert_eq!(fx.sink.stats().snapshot()[1], 0, "no drops");
}

/// Linux `evtest` analog: south button + left stick on the virtio gamepad.
#[test]
fn gamepad_events_match_evtest_codes() {
    let mut fx = Fx::new(InputKind::Gamepad);
    assert_eq!(InputKind::Gamepad.name(), "Ternvale Virtio Gamepad");
    fx.post(0, 0, 8, WRITE);
    fx.kick(0);
    assert!(fx.sink.key(BTN_SOUTH, true));
    assert!(fx.sink.abs_axis(ABS_X, 16_384));
    assert!(fx.sink.abs_axis(ABS_Y, -1000));
    assert_eq!(fx.event(0, 0), InputEvent::key(BTN_SOUTH, true));
    assert_eq!(fx.event(0, 1), InputEvent::syn());
    assert_eq!(fx.event(0, 2), InputEvent::abs(ABS_X, 16_384u32));
    assert_eq!(fx.event(0, 3), InputEvent::syn());
    assert_eq!(
        fx.event(0, 4),
        InputEvent::abs(ABS_Y, (-1000i32) as u32),
        "signed axis as two's complement (evtest shows -1000)"
    );
    assert_eq!(fx.event(0, 5), InputEvent::syn());
    assert_eq!(fx.sink.stats().snapshot(), [6, 0, 0]);
}
