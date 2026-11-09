//! virtio-fs request queue through virtio-mmio.

use std::sync::Arc;
use std::time::Duration;

use ternvale_devices::VirtioMmio;
use ternvale_virtiofs::{Options, VirtioFs};

use crate::guest::{scratch, Guest};
use crate::quiet;
use crate::transport::{bring_up, extra_mmio, notify};

/// Kick the request queue with a FUSE in-header plus args.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Some(guest) = Guest::new() else {
        return;
    };
    let dir = scratch("fs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let opened = VirtioFs::open(
        dir.clone(),
        "fuzz",
        Options {
            read_only: false,
            max_handles: 8,
            windows_names: false,
        },
        Arc::clone(&guest.memory),
        Arc::clone(&guest.irq),
    );
    let Ok(fs) = opened else {
        crate::guest::cleanup(&dir);
        return;
    };
    let req = guest.payload();
    guest.write(req, &data.iter().copied().take(256).collect::<Vec<_>>());
    guest.write_desc(0, req, data.len().min(256) as u32, 1, 1);
    guest.write_desc(1, req + 256, 128, 2, 0);
    guest.publish(&[0]);
    let mut mmio = VirtioMmio::with_irq(0, Box::new(fs), Arc::clone(&guest.irq));
    bring_up(&mut mmio, &guest, 2);
    notify(&mut mmio, 1);
    extra_mmio(&mut mmio, data.get(40..).unwrap_or(&[]));
    guest.wait_used(1, Duration::from_millis(20));
    drop(mmio);
    crate::guest::cleanup(&dir);
}
