//! `--display window` input: virtio-input keyboard, tablet, relative mouse,
//! and gamepad whose events come from the dev viewer window.
//!
//! Devices go right after the virtio-gpu (next virtio-mmio slots, or the next
//! PCI functions). [`GuestInput`] turns the viewer's messages into Linux
//! input events: macOS key codes through the device crate's keymap (with an
//! optional Command remap), buttons to `BTN_*`, the wheel to `REL_WHEEL` /
//! `REL_HWHEEL`, captured motion to the mouse, and GameController events to
//! the gamepad.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ternvale_config::{CmdMap, VirtioTransport};
use ternvale_devices::{
    mac_to_linux_key, InputKind, InputSink, VirtioInput, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT,
};
use ternvale_gpu::{InputTarget, PointerButton, ViewerInput};
use ternvale_vmm::{DeviceAttach, MachineError};

use crate::display::Mapped;

/// The keyboard, tablet, mouse, and gamepad the viewer drives.
pub struct GuestInput {
    keyboard: InputSink,
    tablet: InputSink,
    mouse: InputSink,
    gamepad: InputSink,
    cmd_map: CmdMap,
    relative: Arc<AtomicBool>,
}

impl GuestInput {
    /// Drive the four virtio-input sinks.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn new(
        keyboard: InputSink,
        tablet: InputSink,
        mouse: InputSink,
        gamepad: InputSink,
        cmd_map: CmdMap,
        relative: Arc<AtomicBool>,
    ) -> Self {
        Self {
            keyboard,
            tablet,
            mouse,
            gamepad,
            cmd_map,
            relative,
        }
    }
}

impl InputTarget for GuestInput {
    fn deliver(&self, input: ViewerInput) {
        match input {
            ViewerInput::Key {
                mac_keycode,
                pressed,
            } => match map_key(mac_keycode, self.cmd_map) {
                Some(code) => {
                    self.keyboard.key(code, pressed);
                }
                None => tracing::warn!(
                    target: "ternvale::cli",
                    mac_keycode = %format!("{mac_keycode:#x}"),
                    pressed,
                    "viewer key has no Linux mapping; event dropped"
                ),
            },
            ViewerInput::Pointer { x, y } => {
                self.tablet.move_to(x, y);
            }
            ViewerInput::Button { button, pressed } => {
                let code = match button {
                    PointerButton::Left => BTN_LEFT,
                    PointerButton::Right => BTN_RIGHT,
                    PointerButton::Middle => BTN_MIDDLE,
                };
                self.tablet.key(code, pressed);
                self.mouse.key(code, pressed);
            }
            ViewerInput::Scroll { dx, dy } => {
                self.tablet.scroll(dx, dy);
                self.mouse.scroll(dx, dy);
            }
            ViewerInput::RelMotion { dx, dy } => {
                self.mouse.rel_xy(dx, dy);
            }
            ViewerInput::Capture { on } => {
                self.relative.store(on, Ordering::Relaxed);
                tracing::info!(target: "ternvale::cli", captured = on, "pointer capture from viewer");
            }
            ViewerInput::GamepadButton { code, pressed } => {
                self.gamepad.key(code, pressed);
            }
            ViewerInput::GamepadAxis { code, value } => {
                self.gamepad.abs_axis(code, value);
            }
        }
    }

    fn relative_wanted(&self) -> bool {
        self.relative.load(Ordering::Relaxed)
    }
}

fn map_key(mac_keycode: u16, cmd_map: CmdMap) -> Option<u16> {
    let code = mac_to_linux_key(mac_keycode)?;
    Some(match (cmd_map, code) {
        (CmdMap::CmdCtrl, 125) => 29, // Left Command -> KEY_LEFTCTRL
        (CmdMap::CmdCtrl, 126) => 97, // Right Command -> KEY_RIGHTCTRL
        (CmdMap::CmdAlt, 125) => 56,  // Left Command -> KEY_LEFTALT
        (CmdMap::CmdAlt, 126) => 100, // Right Command -> KEY_RIGHTALT
        _ => code,
    })
}

/// Add keyboard, tablet, mouse, and gamepad after the GPU.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(slot, transport = %transport))]
pub fn attach_input(
    attach: &DeviceAttach,
    transport: VirtioTransport,
    slot: u32,
    cmd_map: CmdMap,
    relative: Arc<AtomicBool>,
) -> Result<(Vec<Mapped>, Arc<GuestInput>), MachineError> {
    let mut mapped = Vec::new();
    let mut sinks = Vec::new();
    let kinds = [
        InputKind::Keyboard,
        InputKind::Tablet,
        InputKind::Mouse,
        InputKind::Gamepad,
    ];
    for (offset, kind) in kinds.into_iter().enumerate() {
        let offset = u32::try_from(offset)
            .map_err(|_| MachineError::Attach("virtio-input offset does not fit in u32".into()))?;
        if transport == VirtioTransport::Pci {
            let (bdf, sink) = VirtioInput::attach_pci(attach, kind)?;
            tracing::info!(target: "ternvale::cli", %bdf, device = kind.short(), "attached virtio-input");
            sinks.push(sink);
            continue;
        }
        let slot = slot + offset;
        let input = VirtioInput::attach(
            slot,
            kind,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(slot),
        )
        .map_err(|error| MachineError::Attach(format!("virtio-input {}: {error}", kind.short())))?;
        tracing::info!(target: "ternvale::cli", slot, device = kind.short(),
            base = %format!("{:#x}", input.base), "attached virtio-input");
        mapped.push((input.base, input.size, input.device));
        sinks.push(input.sink);
    }
    let gamepad = sinks.pop();
    let mouse = sinks.pop();
    let tablet = sinks.pop();
    let keyboard = sinks.pop();
    match (keyboard, tablet, mouse, gamepad) {
        (Some(keyboard), Some(tablet), Some(mouse), Some(gamepad)) => Ok((
            mapped,
            Arc::new(GuestInput::new(
                keyboard, tablet, mouse, gamepad, cmd_map, relative,
            )),
        )),
        _ => Err(MachineError::Attach(
            "virtio-input keyboard, tablet, mouse, and gamepad were not all attached".into(),
        )),
    }
}

#[cfg(test)]
#[path = "display_input_tests.rs"]
mod tests;
