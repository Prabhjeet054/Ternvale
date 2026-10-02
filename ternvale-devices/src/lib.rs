//! Emulated and virtio devices for Ternvale guests.

mod expect;
mod uart;
mod virtio;

pub use expect::{last_lines, Progress, Session, Step};
pub use uart::{ByteSink, Pl011, StdoutFile, UartError, PL011_BASE, PL011_SIZE};
pub use virtio::{
    attach_disks, slot_base, AttachedBlk, BlkStats, Buffer, Chain, FixedDevice, IrqHook,
    QueueNotify, SplitQueue, VirtioBlk, VirtioBlkError, VirtioDevice, VirtioIrq, VirtioMmio,
    VirtioMmioError, STATUS_ACKNOWLEDGE, STATUS_DEVICE_NEEDS_RESET, STATUS_DRIVER,
    STATUS_DRIVER_OK, STATUS_FAILED, STATUS_FEATURES_OK, VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_RO,
    VIRTIO_BLK_ID, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC, VIRTIO_F_VERSION_1,
};
pub use virtio::{
    open_backend, summarize_frame, AttachedNet, LoopbackBackend, NetBackend, NetError, NetStats,
    PacketKind, PacketSummary, PcapWriter, VirtioNet, DEFAULT_MAC, GATEWAY_IP, GATEWAY_MAC,
    MAX_FRAME, NET_HDR_LEN, PCAP_ENV, VIRTIO_NET_F_MAC, VIRTIO_NET_F_MRG_RXBUF,
    VIRTIO_NET_F_STATUS, VIRTIO_NET_ID, VIRTIO_NET_S_LINK_UP,
};
#[cfg(feature = "vmnet")]
pub use virtio::{VmnetBackend, VMNET_SHARED_MODE};

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
