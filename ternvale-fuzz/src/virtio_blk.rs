//! virtio-blk request handler through virtio-mmio.

use std::sync::Arc;
use std::time::Duration;

use ternvale_devices::{VirtioBlk, VirtioMmio};
use ternvale_metrics::Metrics;

use crate::guest::{scratch, Guest};
use crate::quiet;
use crate::transport::{bring_up, extra_mmio, notify};

const HDR: u64 = 16;
const SECTOR: u32 = 512;

/// Open a tiny image, program the queue, notify.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Some(guest) = Guest::new() else {
        return;
    };
    let dir = scratch("blk");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let image = dir.join("disk.img");
    if std::fs::write(&image, vec![0u8; 4096]).is_err() {
        crate::guest::cleanup(&dir);
        return;
    }
    let opened = VirtioBlk::open(
        &image,
        false,
        Arc::clone(&guest.memory),
        Arc::clone(&guest.irq),
        Arc::new(Metrics::new()),
    );
    let Ok(blk) = opened else {
        crate::guest::cleanup(&dir);
        return;
    };
    place_request(&guest, data);
    let mut mmio = VirtioMmio::with_irq(0, Box::new(blk), Arc::clone(&guest.irq));
    bring_up(&mut mmio, &guest, 1);
    notify(&mut mmio, 0);
    extra_mmio(&mut mmio, data.get(32..).unwrap_or(&[]));
    guest.wait_used(1, Duration::from_millis(20));
    drop(mmio);
    crate::guest::cleanup(&dir);
}

fn place_request(guest: &Guest, data: &[u8]) {
    let hdr = guest.payload();
    let ty = u32::from(data.first().copied().unwrap_or(0) % 9);
    let sector = u64::from(data.get(1).copied().unwrap_or(0) % 4);
    guest.write(hdr, &ty.to_le_bytes());
    guest.write(hdr + 4, &[0; 4]);
    guest.write(hdr + 8, &sector.to_le_bytes());
    let buf = hdr + HDR;
    let payload: Vec<u8> = data
        .get(2..)
        .unwrap_or(&[])
        .iter()
        .copied()
        .take(SECTOR as usize)
        .collect();
    guest.write(buf, &payload);
    guest.write(buf + u64::from(SECTOR), &[0xff]);
    guest.write_desc(0, hdr, HDR as u32, 1, 1);
    guest.write_desc(1, buf, SECTOR, 1 | 2, 2);
    guest.write_desc(2, buf + u64::from(SECTOR), 1, 2, 0);
    guest.publish(&[0]);
}
