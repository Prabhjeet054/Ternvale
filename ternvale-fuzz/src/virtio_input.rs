//! virtio-input status queue through virtio-mmio.

use std::sync::Arc;

use ternvale_devices::{InputKind, VirtioInput, VirtioMmio};

use crate::guest::Guest;
use crate::quiet;
use crate::transport::{bring_up, extra_mmio, notify};

/// Kick statusq with a guest-shaped event buffer.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Some(guest) = Guest::new() else {
        return;
    };
    let kind = if data.first().copied().unwrap_or(0) & 1 == 0 {
        InputKind::Keyboard
    } else {
        InputKind::Tablet
    };
    let input = VirtioInput::new(kind, Arc::clone(&guest.memory), Arc::clone(&guest.irq));
    guest.write(
        guest.payload(),
        &data.iter().copied().take(64).collect::<Vec<_>>(),
    );
    guest.write_desc(0, guest.payload(), data.len().min(64) as u32, 0, 0);
    guest.publish(&[0]);
    let mut mmio = VirtioMmio::with_irq(0, Box::new(input), Arc::clone(&guest.irq));
    bring_up(&mut mmio, &guest, 2);
    notify(&mut mmio, 1);
    extra_mmio(&mut mmio, data.get(8..).unwrap_or(&[]));
}
