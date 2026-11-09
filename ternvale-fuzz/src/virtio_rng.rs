//! virtio-rng request handler through virtio-mmio.

use std::sync::Arc;

use ternvale_devices::{VirtioMmio, VirtioRng};

use crate::guest::Guest;
use crate::quiet;
use crate::transport::{bring_up, extra_mmio, notify};

/// Kick a writable entropy buffer.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Some(guest) = Guest::new() else {
        return;
    };
    let rng = VirtioRng::new(Arc::clone(&guest.memory), Arc::clone(&guest.irq));
    let len = u32::from(data.first().copied().unwrap_or(16)).clamp(1, 4096);
    guest.write_desc(0, guest.payload(), len, 2, 0);
    guest.publish(&[0]);
    let mut mmio = VirtioMmio::with_irq(0, Box::new(rng), Arc::clone(&guest.irq));
    bring_up(&mut mmio, &guest, 1);
    notify(&mut mmio, 0);
    extra_mmio(&mut mmio, data.get(1..).unwrap_or(&[]));
}
