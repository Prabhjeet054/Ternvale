//! Emulated and virtio devices for Ternvale guests.

mod expect;
mod uart;
mod virtio;

pub use expect::{last_lines, Progress, Session, Step};
pub use uart::{ByteSink, Pl011, StdoutFile, UartError, PL011_BASE, PL011_SIZE};
pub use virtio::{
    slot_base, AttachedBlk, BlkStats, Buffer, Chain, FixedDevice, IrqHook, QueueNotify, SplitQueue,
    VirtioBlk, VirtioBlkError, VirtioDevice, VirtioIrq, VirtioMmio, VirtioMmioError,
    STATUS_ACKNOWLEDGE, STATUS_DEVICE_NEEDS_RESET, STATUS_DRIVER, STATUS_DRIVER_OK, STATUS_FAILED,
    STATUS_FEATURES_OK, VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_RO, VIRTIO_BLK_ID, VIRTIO_F_EVENT_IDX,
    VIRTIO_F_INDIRECT_DESC, VIRTIO_F_VERSION_1,
};

#[cfg(test)]
#[path = "boot_hv_test.rs"]
mod boot_hv_test;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-devices");
    }
}
