//! `--display window` input: a virtio-input keyboard and tablet whose events
//! come from the dev viewer window.
//!
//! Both devices go right after the virtio-gpu (next two virtio-mmio slots,
//! or the next PCI functions). [`GuestInput`] turns the viewer's messages into
//! Linux input events: macOS key codes through the device crate's keymap,
//! buttons to `BTN_*`, the wheel to `REL_WHEEL` / `REL_HWHEEL`.

use std::sync::Arc;

use ternvale_config::VirtioTransport;
use ternvale_devices::{
    mac_to_linux_key, InputKind, InputSink, VirtioInput, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT,
};
use ternvale_gpu::{InputTarget, PointerButton, ViewerInput};
use ternvale_vmm::{DeviceAttach, MachineError};

use crate::display::Mapped;

/// The keyboard and tablet the viewer drives.
pub struct GuestInput {
    keyboard: InputSink,
    tablet: InputSink,
}

impl GuestInput {
    /// Drive `keyboard` and `tablet`.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn new(keyboard: InputSink, tablet: InputSink) -> Self {
        Self { keyboard, tablet }
    }
}

impl InputTarget for GuestInput {
    fn deliver(&self, input: ViewerInput) {
        match input {
            ViewerInput::Key {
                mac_keycode,
                pressed,
            } => match mac_to_linux_key(mac_keycode) {
                Some(code) => {
                    self.keyboard.key(code, pressed);
                }
                None => tracing::debug!(target: "ternvale::cli",
                    mac_keycode = %format!("{mac_keycode:#x}"), pressed,
                    "viewer key has no Linux mapping; ignored"),
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
            }
            ViewerInput::Scroll { dx, dy } => {
                self.tablet.scroll(dx, dy);
            }
        }
    }
}

/// Add the keyboard and tablet: on `slot` and `slot + 1` for virtio-mmio,
/// else on the next two PCI functions. Returns the MMIO devices to map
/// (none for PCI) and the input target.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(slot, transport = %transport))]
pub fn attach_input(
    attach: &DeviceAttach,
    transport: VirtioTransport,
    slot: u32,
) -> Result<(Vec<Mapped>, Arc<GuestInput>), MachineError> {
    let mut mapped = Vec::new();
    let mut sinks = Vec::new();
    for (offset, kind) in [(0, InputKind::Keyboard), (1, InputKind::Tablet)] {
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
    let tablet = sinks.pop();
    let keyboard = sinks.pop();
    match (keyboard, tablet) {
        (Some(keyboard), Some(tablet)) => Ok((mapped, Arc::new(GuestInput::new(keyboard, tablet)))),
        _ => Err(MachineError::Attach(
            "virtio-input keyboard and tablet were not both attached".into(),
        )),
    }
}

#[cfg(test)]
#[path = "display_input_tests.rs"]
mod tests;
