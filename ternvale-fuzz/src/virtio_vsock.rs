//! virtio-vsock TX handler through virtio-mmio.

use std::sync::Arc;
use std::time::Duration;

use ternvale_devices::{VirtioMmio, VirtioVsock, VsockConfig, VSOCK_HDR_LEN};

use crate::guest::{scratch, Guest};
use crate::quiet;
use crate::transport::{bring_up, extra_mmio, notify};

/// Kick TX with a vsock header plus payload.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Some(guest) = Guest::new() else {
        return;
    };
    let dir = scratch("vsock");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let config = VsockConfig {
        guest_cid: None,
        uds_dir: dir.clone(),
        listen_ports: Vec::new(),
    };
    let opened = VirtioVsock::open(&config, Arc::clone(&guest.memory), Arc::clone(&guest.irq));
    let Ok(vsock) = opened else {
        crate::guest::cleanup(&dir);
        return;
    };
    let hdr = guest.payload();
    let mut packet = vec![0u8; VSOCK_HDR_LEN];
    let copy = data.len().min(VSOCK_HDR_LEN);
    packet[..copy].copy_from_slice(&data[..copy]);
    guest.write(hdr, &packet);
    guest.write(
        hdr + VSOCK_HDR_LEN as u64,
        data.get(VSOCK_HDR_LEN..).unwrap_or(&[]),
    );
    let len = (VSOCK_HDR_LEN + data.len().saturating_sub(VSOCK_HDR_LEN).min(256)) as u32;
    guest.write_desc(0, hdr, len, 0, 0);
    guest.publish(&[0]);
    let mut mmio = VirtioMmio::with_irq(0, Box::new(vsock), Arc::clone(&guest.irq));
    bring_up(&mut mmio, &guest, 3);
    notify(&mut mmio, 1);
    extra_mmio(&mut mmio, data.get(8..).unwrap_or(&[]));
    guest.wait_used(1, Duration::from_millis(20));
    drop(mmio);
    crate::guest::cleanup(&dir);
}
