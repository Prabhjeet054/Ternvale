//! Program virtio-mmio the way a guest driver would.

use ternvale_devices::{
    VirtioMmio, STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK, STATUS_FEATURES_OK,
};

use crate::guest::{Guest, QUEUE_SIZE};

const MAGIC: u64 = 0x000;
const VERSION: u64 = 0x004;
const DEV_FEAT: u64 = 0x010;
const DEV_FEAT_SEL: u64 = 0x014;
const DRV_FEAT: u64 = 0x020;
const DRV_FEAT_SEL: u64 = 0x024;
const QUEUE_SEL: u64 = 0x030;
const QUEUE_NUM: u64 = 0x038;
const QUEUE_READY: u64 = 0x044;
const QUEUE_NOTIFY: u64 = 0x050;
const STATUS: u64 = 0x070;
const DESC_LO: u64 = 0x080;
const DESC_HI: u64 = 0x084;
const AVAIL_LO: u64 = 0x090;
const AVAIL_HI: u64 = 0x094;
const USED_LO: u64 = 0x0a0;
const USED_HI: u64 = 0x0a4;
const CONFIG: u64 = 0x100;

/// Reset, offer features, program queues, DRIVER_OK, then notify.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(queues))]
pub fn bring_up(mmio: &mut VirtioMmio, guest: &Guest, queues: u16) {
    let _ = mmio.read(MAGIC, 4);
    let _ = mmio.read(VERSION, 4);
    mmio.write(STATUS, 4, 0);
    mmio.write(STATUS, 4, u64::from(STATUS_ACKNOWLEDGE));
    mmio.write(STATUS, 4, u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER));
    mmio.write(DEV_FEAT_SEL, 4, 0);
    let lo = mmio.read(DEV_FEAT, 4);
    mmio.write(DRV_FEAT_SEL, 4, 0);
    mmio.write(DRV_FEAT, 4, lo);
    mmio.write(DEV_FEAT_SEL, 4, 1);
    let hi = mmio.read(DEV_FEAT, 4);
    mmio.write(DRV_FEAT_SEL, 4, 1);
    mmio.write(DRV_FEAT, 4, hi);
    mmio.write(
        STATUS,
        4,
        u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK),
    );
    for queue in 0..queues.min(4) {
        program_queue(mmio, guest, queue);
    }
    mmio.write(
        STATUS,
        4,
        u64::from(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK),
    );
}

/// Select `queue` and point it at the fake rings.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(queue))]
pub fn program_queue(mmio: &mut VirtioMmio, guest: &Guest, queue: u16) {
    mmio.write(QUEUE_SEL, 4, u64::from(queue));
    mmio.write(QUEUE_NUM, 4, u64::from(QUEUE_SIZE));
    write64(mmio, DESC_LO, DESC_HI, guest.desc());
    write64(mmio, AVAIL_LO, AVAIL_HI, guest.avail());
    write64(mmio, USED_LO, USED_HI, guest.used());
    mmio.write(QUEUE_READY, 4, 1);
}

/// QueueNotify for `queue`.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(queue))]
pub fn notify(mmio: &mut VirtioMmio, queue: u16) {
    mmio.write(QUEUE_NOTIFY, 4, u64::from(queue));
}

/// Extra guest MMIO (config and register) from leftover fuzz bytes.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn extra_mmio(mmio: &mut VirtioMmio, data: &[u8]) {
    for chunk in data.chunks(6).take(16) {
        if chunk.len() < 3 {
            break;
        }
        let offset = u16::from_le_bytes([chunk[0], chunk[1]]) as u64 % 0x200;
        let size = match chunk.get(2).copied().unwrap_or(4) % 3 {
            0 => 1,
            1 => 2,
            _ => 4,
        };
        let value = chunk.get(3..).map(read_u64).unwrap_or(0);
        if chunk[0] & 1 == 0 {
            let _ = mmio.read(offset.max(CONFIG), size);
        } else {
            mmio.write(offset, size, value);
        }
    }
}

fn write64(mmio: &mut VirtioMmio, lo: u64, hi: u64, value: u64) {
    mmio.write(lo, 4, value & 0xffff_ffff);
    mmio.write(hi, 4, value >> 32);
}

fn read_u64(bytes: &[u8]) -> u64 {
    let mut raw = [0u8; 8];
    let n = bytes.len().min(8);
    raw[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(raw)
}
