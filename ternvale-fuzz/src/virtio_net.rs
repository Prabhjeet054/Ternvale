//! virtio-net TX handler through virtio-mmio (loopback backend).

use std::sync::Arc;
use std::time::Duration;

use ternvale_devices::{LoopbackBackend, VirtioMmio, VirtioNet, DEFAULT_MAC};
use ternvale_metrics::Metrics;

use crate::guest::Guest;
use crate::quiet;
use crate::transport::{bring_up, extra_mmio, notify};

const NET_HDR: u32 = 12;

/// Kick the TX queue with a guest-shaped Ethernet frame.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Some(guest) = Guest::new() else {
        return;
    };
    let opened = VirtioNet::open(
        DEFAULT_MAC,
        Box::new(LoopbackBackend::new()),
        Arc::clone(&guest.memory),
        Arc::clone(&guest.irq),
        None,
        Arc::new(Metrics::new()),
    );
    let Ok(net) = opened else {
        return;
    };
    let hdr = guest.payload();
    guest.write(hdr, &[0; NET_HDR as usize]);
    guest.write(
        hdr + u64::from(NET_HDR),
        &data.iter().copied().take(512).collect::<Vec<_>>(),
    );
    let frame_len = NET_HDR + data.len().min(512) as u32;
    guest.write_desc(0, hdr, frame_len, 0, 0);
    guest.publish(&[0]);
    let mut mmio = VirtioMmio::with_irq(0, Box::new(net), Arc::clone(&guest.irq));
    bring_up(&mut mmio, &guest, 2);
    notify(&mut mmio, 1);
    extra_mmio(&mut mmio, data.get(16..).unwrap_or(&[]));
    guest.wait_used(1, Duration::from_millis(20));
}
