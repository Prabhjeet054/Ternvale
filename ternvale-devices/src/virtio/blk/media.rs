//! The medium a virtio-blk device presents: its feature bits and device
//! config space (virtio 1.2, 5.2.3 "Feature bits" and 5.2.4 "Device
//! configuration layout").
//!
//! Optical media ([`BlkMedia::Optical`], the `[iso]` slots). virtio-blk has
//! no CD-ROM device type or media-type field. The device ID stays 2
//! (modern PCI 0x1042), so Windows' `viostor` binds to it as it would to any
//! virtio disk. A real CD/DVD drive (SCSI peripheral type 05h, MMC commands)
//! would need virtio-scsi (device 8). What makes the device "CD-ROM-like" is
//! the feature set:
//! - `VIRTIO_BLK_F_RO` (5.2.3): read-only. The driver must not write, and
//!   writes fail with `VIRTIO_BLK_S_IOERR`.
//! - `VIRTIO_BLK_F_BLK_SIZE` with `blk_size` = 2048 (5.2.4): the 2048-byte
//!   logical sector of ISO 9660 (ECMA-119 6.1.2) and of UDF on DVD. Linux uses
//!   it as the logical block size. EDK2's `VirtioBlkDxe` uses it as the
//!   BlockIo block size, which its El Torito and UDF code accept. Request
//!   sector numbers stay in 512-byte units (5.2.6), so the request path is
//!   unchanged.
//! - No `VIRTIO_BLK_F_FLUSH`: nothing to flush on read-only media.
//!
//! TODO(verify): the exact flag set Windows Setup needs from optical media on
//! virtio-blk. Check: (1) `viostor` (virtio-win `viostor/virtio_stor.c`)
//! negotiates `BLK_SIZE` and reports 2048 as the READ CAPACITY block length;
//! (2) Windows mounts the UDF volume of a Windows ISO on a disk-class device
//! with 2048-byte sectors; (3) Setup finds `sources\install.wim` on it. The
//! fallbacks are `blk_size` = 512 (QEMU's virtio-blk default) or a virtio-scsi
//! CD-ROM.

/// virtio-blk `VIRTIO_BLK_F_BLK_SIZE`: `blk_size` in config space is valid.
pub const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 6;
/// `blk_size` of optical media.
pub const OPTICAL_BLOCK_SIZE: u32 = 2048;

/// Bytes of `struct virtio_blk_config` the device fills: `capacity` (le64 at
/// 0), `size_max`, `seg_max`, `geometry` (unset), then `blk_size` (le32 at 20).
const CONFIG_BYTES: usize = 24;
const BLK_SIZE_OFFSET: usize = 20;

/// What the guest is told the image is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlkMedia {
    /// A disk, writable unless `read_only`.
    Disk {
        /// Offer `VIRTIO_BLK_F_RO` and fail writes.
        read_only: bool,
    },
    /// A read-only optical disc image (ISO 9660 / UDF).
    Optical,
}

impl BlkMedia {
    /// True when the guest must not write.
    #[tracing::instrument(level = "trace", target = "ternvale::virtio::blk", skip_all)]
    pub fn read_only(self) -> bool {
        match self {
            Self::Disk { read_only } => read_only,
            Self::Optical => true,
        }
    }

    /// `"disk"` or `"optical"`, for logs.
    #[tracing::instrument(level = "trace", target = "ternvale::virtio::blk", skip_all)]
    pub fn name(self) -> &'static str {
        match self {
            Self::Disk { .. } => "disk",
            Self::Optical => "optical",
        }
    }

    /// `blk_size` the device offers, if any.
    #[tracing::instrument(level = "trace", target = "ternvale::virtio::blk", skip_all)]
    pub fn block_size(self) -> Option<u32> {
        match self {
            Self::Disk { .. } => None,
            Self::Optical => Some(OPTICAL_BLOCK_SIZE),
        }
    }

    /// Device feature bits (virtio-blk ones; transport bits are added by the
    /// core).
    #[tracing::instrument(level = "trace", target = "ternvale::virtio::blk", skip_all)]
    pub fn features(self) -> u64 {
        match self {
            Self::Disk { read_only: false } => super::VIRTIO_BLK_F_FLUSH,
            Self::Disk { read_only: true } => super::VIRTIO_BLK_F_FLUSH | super::VIRTIO_BLK_F_RO,
            Self::Optical => super::VIRTIO_BLK_F_RO | VIRTIO_BLK_F_BLK_SIZE,
        }
    }

    /// Device config space for an image of `capacity` 512-byte sectors.
    pub(super) fn config_space(self, capacity: u64) -> [u8; CONFIG_BYTES] {
        let mut bytes = [0u8; CONFIG_BYTES];
        bytes[..8].copy_from_slice(&capacity.to_le_bytes());
        if let Some(size) = self.block_size() {
            bytes[BLK_SIZE_OFFSET..BLK_SIZE_OFFSET + 4].copy_from_slice(&size.to_le_bytes());
        }
        bytes
    }
}
