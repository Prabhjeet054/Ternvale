//! On-disk magic numbers and header layouts.

/// Stack manifest magic (`TVST`).
pub const STACK_MAGIC: [u8; 4] = *b"TVST";
/// Delta layer magic (`TVDS`).
pub const DELTA_MAGIC: [u8; 4] = *b"TVDS";

/// Supported stack manifest version.
pub const STACK_VERSION: u32 = 1;
/// Supported delta file version.
pub const DELTA_VERSION: u32 = 1;

/// Default COW block size (64 KiB). See `docs/SNAPSHOT.md`.
pub const DEFAULT_BLOCK_SIZE: u32 = 65_536;

/// Fixed delta header size in bytes.
pub const DELTA_HEADER_LEN: usize = 64;

/// Delta file header (little-endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaHeader {
    /// Logical image size in bytes (multiple of `block_size`).
    pub virtual_size: u64,
    /// COW block size (must match stack).
    pub block_size: u32,
    /// Byte offset of the allocation bitmap.
    pub bitmap_offset: u64,
    /// Length of the bitmap in bytes.
    pub bitmap_bytes: u64,
    /// Start of packed block payloads.
    pub data_offset: u64,
}

impl DeltaHeader {
    /// Encode to on-disk form.
    #[must_use]
    pub fn encode(&self) -> [u8; DELTA_HEADER_LEN] {
        let mut out = [0u8; DELTA_HEADER_LEN];
        out[0..4].copy_from_slice(&DELTA_MAGIC);
        out[4..8].copy_from_slice(&DELTA_VERSION.to_le_bytes());
        out[8..12].copy_from_slice(&self.block_size.to_le_bytes());
        out[12..20].copy_from_slice(&self.virtual_size.to_le_bytes());
        out[20..28].copy_from_slice(&self.bitmap_offset.to_le_bytes());
        out[28..36].copy_from_slice(&self.bitmap_bytes.to_le_bytes());
        out[36..44].copy_from_slice(&self.data_offset.to_le_bytes());
        out
    }

    /// Parse a delta header; rejects bad magic or unsupported version.
    pub fn decode(bytes: &[u8]) -> Result<Self, crate::SnapshotError> {
        Self::decode_at(bytes, None)
    }

    /// Like [`decode`], with an optional path for clearer `BadMagic` errors.
    pub fn decode_at(
        bytes: &[u8],
        path: Option<&std::path::Path>,
    ) -> Result<Self, crate::SnapshotError> {
        if bytes.len() < DELTA_HEADER_LEN {
            return Err(crate::SnapshotError::Truncated {
                what: "delta header",
            });
        }
        if bytes[0..4] != DELTA_MAGIC {
            return Err(crate::SnapshotError::BadMagic {
                path: path.map(std::path::Path::to_path_buf),
                expected: "TVDS",
            });
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().expect("version"));
        if version != DELTA_VERSION {
            return Err(crate::SnapshotError::UnsupportedVersion {
                kind: "delta",
                found: version,
                supported: DELTA_VERSION,
            });
        }
        let block_size = u32::from_le_bytes(bytes[8..12].try_into().expect("block"));
        let virtual_size = u64::from_le_bytes(bytes[12..20].try_into().expect("size"));
        let bitmap_offset = u64::from_le_bytes(bytes[20..28].try_into().expect("bm_off"));
        let bitmap_bytes = u64::from_le_bytes(bytes[28..36].try_into().expect("bm_len"));
        let data_offset = u64::from_le_bytes(bytes[36..44].try_into().expect("data"));
        Ok(Self {
            virtual_size,
            block_size,
            bitmap_offset,
            bitmap_bytes,
            data_offset,
        })
    }

    /// Number of COW blocks covering `virtual_size`.
    #[must_use]
    pub fn block_count(&self) -> u64 {
        let bs = u64::from(self.block_size);
        if bs == 0 {
            return 0;
        }
        self.virtual_size.div_ceil(bs)
    }
}

/// Bitmap helpers (one bit per COW block).
pub fn bitmap_bytes_for_blocks(block_count: u64) -> u64 {
    block_count.div_ceil(8)
}

pub fn bitmap_test(bitmap: &[u8], block_index: u64) -> bool {
    let byte = (block_index / 8) as usize;
    let bit = 1u8 << (block_index % 8);
    bitmap.get(byte).is_some_and(|b| b & bit != 0)
}

pub fn bitmap_set(bitmap: &mut [u8], block_index: u64) {
    let byte = (block_index / 8) as usize;
    let bit = 1u8 << (block_index % 8);
    if let Some(b) = bitmap.get_mut(byte) {
        *b |= bit;
    }
}

/// Rank: how many set bits exist strictly before `block_index`.
pub fn bitmap_rank_before(bitmap: &[u8], block_index: u64) -> u64 {
    let mut rank = 0u64;
    let full_bytes = (block_index / 8) as usize;
    for byte in bitmap.iter().take(full_bytes) {
        rank += byte.count_ones() as u64;
    }
    let remainder = block_index % 8;
    if remainder > 0 {
        let mask = (1u8 << remainder) - 1;
        if let Some(byte) = bitmap.get(full_bytes) {
            rank += (byte & mask).count_ones() as u64;
        }
    }
    rank
}
