//! Viewer input to virtio-input routing. No driver binds the event queues
//! here, so every routed report is dropped ("driver not ready") and counted:
//! the drop counters show which device got how many events.

use std::sync::{Arc, Mutex};

use ternvale_devices::{InputKind, VirtioInput, VirtioIrq};
use ternvale_gpu::{InputTarget, PointerButton, ViewerInput};
use ternvale_vmm::GuestMemory;

use super::GuestInput;

fn device(kind: InputKind) -> VirtioInput {
    let memory = Arc::new(Mutex::new(GuestMemory::new().expect("memory")));
    VirtioInput::new(kind, memory, VirtioIrq::new())
}

#[test]
fn viewer_input_reaches_the_right_device() {
    let (keyboard, tablet) = (device(InputKind::Keyboard), device(InputKind::Tablet));
    let (kbd_stats, tab_stats) = (keyboard.sink().stats(), tablet.sink().stats());
    let input = GuestInput::new(keyboard.sink(), tablet.sink());
    let dropped = || (kbd_stats.snapshot()[1], tab_stats.snapshot()[1]);

    input.deliver(ViewerInput::Key {
        mac_keycode: 0x00,
        pressed: true,
    });
    assert_eq!(dropped(), (2, 0), "KEY_A + SYN to the keyboard");
    input.deliver(ViewerInput::Key {
        mac_keycode: 0x3f,
        pressed: true,
    });
    assert_eq!(dropped(), (2, 0), "fn has no mapping and is not sent");
    input.deliver(ViewerInput::Pointer { x: 1, y: 2 });
    assert_eq!(dropped(), (2, 3), "ABS_X, ABS_Y, SYN to the tablet");
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
    assert_eq!(dropped(), (2, 9), "each button is a supported BTN_* code");
    input.deliver(ViewerInput::Scroll { dx: 1, dy: -1 });
    assert_eq!(dropped(), (2, 12), "REL_WHEEL, REL_HWHEEL, SYN");
    input.deliver(ViewerInput::Scroll { dx: 0, dy: 0 });
    assert_eq!(dropped(), (2, 12), "an empty scroll sends nothing");
}
