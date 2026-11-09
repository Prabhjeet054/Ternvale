//! virtio-balloon inflate/stats queues through virtio-mmio.

use std::sync::Arc;
use std::time::Duration;

use ternvale_devices::{BalloonConfig, VirtioBalloon, VirtioMmio};

use crate::guest::Guest;
use crate::quiet;
use crate::transport::{bring_up, extra_mmio, notify};

/// Kick inflate (queue 0) with a PFN list.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Some(guest) = Guest::new() else {
        return;
    };
    let balloon = VirtioBalloon::new(
        Arc::clone(&guest.memory),
        Arc::clone(&guest.irq),
        BalloonConfig {
            ram_mib: 1,
            ram_min_mib: 0,
            metrics: None,
            vcpus: 1,
            watch_pressure: false,
            auto_policy: false,
            reclaim: None,
        },
    );
    guest.write(guest.payload(), data);
    guest.write_desc(0, guest.payload(), data.len().min(64) as u32, 0, 0);
    guest.publish(&[0]);
    let mut mmio = VirtioMmio::with_irq(0, Box::new(balloon), Arc::clone(&guest.irq));
    bring_up(&mut mmio, &guest, 4);
    notify(&mut mmio, 0);
    extra_mmio(&mut mmio, data.get(8..).unwrap_or(&[]));
    guest.wait_used(1, Duration::from_millis(20));
}
