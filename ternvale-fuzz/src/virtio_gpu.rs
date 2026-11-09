//! virtio-gpu control queue through virtio-mmio.

use std::sync::Arc;
use std::time::Duration;

use ternvale_devices::VirtioMmio;
use ternvale_gpu::{GpuConfig, VirtioGpu};

use crate::guest::Guest;
use crate::quiet;
use crate::transport::{bring_up, extra_mmio, notify};

/// Kick controlq with a guest command blob.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Some(guest) = Guest::new() else {
        return;
    };
    let config = GpuConfig {
        modes: vec![(64, 64)],
    };
    let opened = VirtioGpu::new(config, Arc::clone(&guest.memory), Arc::clone(&guest.irq));
    let Ok(gpu) = opened else {
        return;
    };
    let cmd = guest.payload();
    guest.write(cmd, &data.iter().copied().take(256).collect::<Vec<_>>());
    guest.write_desc(0, cmd, data.len().min(256) as u32, 1, 1);
    guest.write_desc(1, cmd + 256, 64, 2, 0);
    guest.publish(&[0]);
    let mut mmio = VirtioMmio::with_irq(0, Box::new(gpu), Arc::clone(&guest.irq));
    bring_up(&mut mmio, &guest, 2);
    notify(&mut mmio, 0);
    extra_mmio(&mut mmio, data.get(24..).unwrap_or(&[]));
    guest.wait_used(1, Duration::from_millis(20));
}
