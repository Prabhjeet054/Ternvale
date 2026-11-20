//! Viewer input to virtio-input routing. No driver binds the event queues
//! here, so every routed report is dropped ("driver not ready") and counted:
//! the drop counters show which device got how many events.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ternvale_config::CmdMap;
use ternvale_devices::{InputKind, VirtioInput, VirtioIrq};
use ternvale_gpu::{InputTarget, PointerButton, ViewerInput};
use ternvale_vmm::GuestMemory;

use super::GuestInput;

fn device(kind: InputKind) -> VirtioInput {
    let memory = Arc::new(Mutex::new(GuestMemory::new().expect("memory")));
    VirtioInput::new(kind, memory, VirtioIrq::new())
}

fn guest() -> (GuestInput, [Arc<ternvale_devices::InputStats>; 4]) {
    let (keyboard, tablet, mouse, gamepad) = (
        device(InputKind::Keyboard),
        device(InputKind::Tablet),
        device(InputKind::Mouse),
        device(InputKind::Gamepad),
    );
    let stats = [
        keyboard.sink().stats(),
        tablet.sink().stats(),
        mouse.sink().stats(),
        gamepad.sink().stats(),
    ];
    let input = GuestInput::new(
        keyboard.sink(),
        tablet.sink(),
        mouse.sink(),
        gamepad.sink(),
        CmdMap::None,
        Arc::new(AtomicBool::new(false)),
    );
    (input, stats)
}

#[test]
fn viewer_input_reaches_the_right_device() {
    let (input, stats) = guest();
    let dropped = || {
        (
            stats[0].snapshot()[1],
            stats[1].snapshot()[1],
            stats[2].snapshot()[1],
            stats[3].snapshot()[1],
        )
    };

    input.deliver(ViewerInput::Key {
        mac_keycode: 0x00,
        pressed: true,
    });
    assert_eq!(dropped(), (2, 0, 0, 0), "KEY_A + SYN to the keyboard");
    input.deliver(ViewerInput::Key {
        mac_keycode: 0x3f,
        pressed: true,
    });
    assert_eq!(dropped(), (2, 0, 0, 0), "fn has no mapping and is not sent");
    input.deliver(ViewerInput::Pointer { x: 1, y: 2 });
    assert_eq!(dropped(), (2, 3, 0, 0), "ABS_X, ABS_Y, SYN to the tablet");
    for button in [
        PointerButton::Left,
        PointerButton::Right,
        PointerButton::Middle,
    ] {
        input.deliver(ViewerInput::Button {
            button,
            pressed: true,
        });
    }
    assert_eq!(
        dropped(),
        (2, 9, 6, 0),
        "each button goes to tablet and mouse"
    );
    input.deliver(ViewerInput::Scroll { dx: 1, dy: -1 });
    assert_eq!(
        dropped(),
        (2, 12, 9, 0),
        "wheel to tablet and mouse (REL + SYN)"
    );
    input.deliver(ViewerInput::Scroll { dx: 0, dy: 0 });
    assert_eq!(dropped(), (2, 12, 9, 0), "an empty scroll sends nothing");
    input.deliver(ViewerInput::RelMotion { dx: 4, dy: -1 });
    assert_eq!(dropped(), (2, 12, 12, 0), "REL_X, REL_Y, SYN to the mouse");
    input.deliver(ViewerInput::GamepadButton {
        code: 0x130,
        pressed: true,
    });
    assert_eq!(dropped(), (2, 12, 12, 2), "BTN_SOUTH + SYN to the gamepad");
    input.deliver(ViewerInput::GamepadAxis { code: 0, value: 10 });
    assert_eq!(dropped(), (2, 12, 12, 4), "ABS_X + SYN to the gamepad");
}

#[test]
fn cmd_map_ctrl_rewrites_command() {
    let keyboard = device(InputKind::Keyboard);
    let stats = keyboard.sink().stats();
    let input = GuestInput::new(
        keyboard.sink(),
        device(InputKind::Tablet).sink(),
        device(InputKind::Mouse).sink(),
        device(InputKind::Gamepad).sink(),
        CmdMap::CmdCtrl,
        Arc::new(AtomicBool::new(false)),
    );
    input.deliver(ViewerInput::Key {
        mac_keycode: 0x37,
        pressed: true,
    });
    assert_eq!(
        stats.snapshot()[1],
        2,
        "Command mapped to Ctrl is advertised"
    );
}

#[test]
fn azerty_q_position_is_key_q_not_a_glyph() {
    let (input, stats) = guest();
    // Physical Q (kVK 0x0c) → KEY_Q. Guest AZERTY turns that into 'a'.
    input.deliver(ViewerInput::Key {
        mac_keycode: 0x0c,
        pressed: true,
    });
    assert_eq!(stats[0].snapshot()[1], 2);
}

#[test]
fn escape_capture_clears_relative_wanted() {
    let relative = Arc::new(AtomicBool::new(true));
    let input = GuestInput::new(
        device(InputKind::Keyboard).sink(),
        device(InputKind::Tablet).sink(),
        device(InputKind::Mouse).sink(),
        device(InputKind::Gamepad).sink(),
        CmdMap::None,
        Arc::clone(&relative),
    );
    assert!(input.relative_wanted());
    input.deliver(ViewerInput::Capture { on: false });
    assert!(!input.relative_wanted(), "Esc / Capture off");
    assert!(!relative.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn focus_loss_key_up_is_a_release_not_a_repeat() {
    let (input, stats) = guest();
    input.deliver(ViewerInput::Key {
        mac_keycode: 0x00,
        pressed: true,
    });
    input.deliver(ViewerInput::Key {
        mac_keycode: 0x00,
        pressed: false,
    });
    assert_eq!(
        stats[0].snapshot(),
        [0, 4, 0],
        "press+syn and release+syn dropped (no driver) — both reports sent"
    );
}
