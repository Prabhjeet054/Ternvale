//! Emulated and virtio devices for Ternvale guests.

mod agent;
mod expect;
mod uart;
mod virtio;

pub use agent::{AgentServer, AgentServerConfig, AgentServerError, AgentState, AgentStatus};
pub use expect::{last_lines, Progress, Session, Step};
pub use uart::{ByteSink, Pl011, StdoutFile, UartError, PL011_BASE, PL011_SIZE};
pub use virtio::{
    attach_blocks, attach_disks, attach_disks_pci, attach_pci, AttachedBlock, AttachedBlocks,
    BlkMedia, BlkPlacement, VirtioPci, OPTICAL_BLOCK_SIZE, VIRTIO_BLK_F_BLK_SIZE,
    VIRTIO_PCI_DEVICE_BASE, VIRTIO_PCI_VENDOR_ID,
};
pub use virtio::{
    guest_socket_path, host_socket_path, vsock_op_name, AttachedVsock, CidLease, VirtioVsock,
    VsockConfig, VsockCounters, VsockError, VsockHeader, VsockStats, FIRST_GUEST_CID, HOST_CID,
    VIRTIO_VSOCK_F_STREAM, VIRTIO_VSOCK_ID, VSOCK_HDR_LEN,
};
pub use virtio::{
    input_type_name, mac_to_linux_key, AttachedInput, InputError, InputEvent, InputKind, InputSink,
    InputStats, VirtioInput, ABS_MAX_VALUE, ABS_X, ABS_Y, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT,
    CFG_ABS_INFO, CFG_EV_BITS, CFG_ID_DEVIDS, CFG_ID_NAME, CFG_ID_SERIAL, CFG_PROP_BITS, CFG_UNSET,
    EV_ABS, EV_KEY, EV_REL, EV_REP, EV_SYN, INPUT_EVENTQ, INPUT_MAX_PENDING, INPUT_STATUSQ,
    REL_HWHEEL, REL_WHEEL, SYN_REPORT, VIRTIO_INPUT_ID,
};
pub use virtio::{
    open_backend, summarize_frame, AttachedNet, LoopbackBackend, NetBackend, NetError, NetStats,
    PacketKind, PacketSummary, PcapWriter, VirtioNet, DEFAULT_MAC, GATEWAY_IP, GATEWAY_MAC,
    MAX_FRAME, NET_HDR_LEN, PCAP_ENV, VIRTIO_NET_F_MAC, VIRTIO_NET_F_MRG_RXBUF,
    VIRTIO_NET_F_STATUS, VIRTIO_NET_ID, VIRTIO_NET_S_LINK_UP,
};
pub use virtio::{
    slot_base, AttachedBlk, BlkStats, Buffer, Chain, FixedDevice, IrqHook, QueueNotify, SplitQueue,
    VirtioBlk, VirtioBlkError, VirtioDevice, VirtioIrq, VirtioMmio, VirtioMmioError,
    STATUS_ACKNOWLEDGE, STATUS_DEVICE_NEEDS_RESET, STATUS_DRIVER, STATUS_DRIVER_OK, STATUS_FAILED,
    STATUS_FEATURES_OK, VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_RO, VIRTIO_BLK_ID, VIRTIO_F_EVENT_IDX,
    VIRTIO_F_INDIRECT_DESC, VIRTIO_F_VERSION_1,
};
pub use virtio::{AttachedRng, RngError, RngStats, VirtioRng, RNG_MAX_PER_CHAIN, VIRTIO_RNG_ID};
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
