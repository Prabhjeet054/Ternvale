//! 2D resources: a host-side pixel copy plus the guest pages backing it.
//!
//! `TRANSFER_TO_HOST_2D` copies from the backing into the host copy row by
//! row. As in QEMU's `virtio_gpu_transfer_to_host_2d`, the backing has the
//! resource's own layout (stride = width × 4), and the row `h` of the
//! rectangle starts at `offset + stride × h`.

use ternvale_vmm::GuestMemory;

use crate::protocol::{MemEntry, Rect};

/// Every 2D format is 32 bits per pixel.
pub const BYTES_PER_PIXEL: u32 = 4;

/// `enum virtio_gpu_formats` (virtio 1.2 §5.7.6.8, `RESOURCE_CREATE_2D`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// `VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM` (1).
    B8g8r8a8,
    /// `VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM` (2), Linux's default.
    B8g8r8x8,
    /// `VIRTIO_GPU_FORMAT_A8R8G8B8_UNORM` (3).
    A8r8g8b8,
    /// `VIRTIO_GPU_FORMAT_X8R8G8B8_UNORM` (4).
    X8r8g8b8,
    /// `VIRTIO_GPU_FORMAT_R8G8B8A8_UNORM` (67).
    R8g8b8a8,
    /// `VIRTIO_GPU_FORMAT_X8B8G8R8_UNORM` (68).
    X8b8g8r8,
    /// `VIRTIO_GPU_FORMAT_A8B8G8R8_UNORM` (121).
    A8b8g8r8,
    /// `VIRTIO_GPU_FORMAT_R8G8B8X8_UNORM` (134).
    R8g8b8x8,
}

impl PixelFormat {
    /// The format for a guest `format` value.
    pub fn from_wire(format: u32) -> Option<Self> {
        Some(match format {
            1 => Self::B8g8r8a8,
            2 => Self::B8g8r8x8,
            3 => Self::A8r8g8b8,
            4 => Self::X8r8g8b8,
            67 => Self::R8g8b8a8,
            68 => Self::X8b8g8r8,
            121 => Self::A8b8g8r8,
            134 => Self::R8g8b8x8,
            _ => return None,
        })
    }

    /// Short name for logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::B8g8r8a8 => "B8G8R8A8",
            Self::B8g8r8x8 => "B8G8R8X8",
            Self::A8r8g8b8 => "A8R8G8B8",
            Self::X8r8g8b8 => "X8R8G8B8",
            Self::R8g8b8a8 => "R8G8B8A8",
            Self::X8b8g8r8 => "X8B8G8R8",
            Self::A8b8g8r8 => "A8B8G8R8",
            Self::R8g8b8x8 => "R8G8B8X8",
        }
    }
}

/// Why a transfer failed. Answered with `RESP_ERR_INVALID_PARAMETER`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransferError {
    /// The source range runs past the end of the backing.
    #[error("bytes {start:#x}..{end:#x} run past the {total:#x}-byte backing")]
    PastBacking {
        /// First source byte.
        start: u64,
        /// One past the last source byte.
        end: u64,
        /// Backing size.
        total: u64,
    },
    /// A backing page is no longer readable guest memory.
    #[error("backing at {addr:#x} is outside guest memory")]
    Unmapped {
        /// Guest physical address.
        addr: u64,
    },
}

/// The guest pages behind a resource, with each entry's start offset.
#[derive(Debug, Clone)]
pub struct Backing {
    entries: Vec<MemEntry>,
    starts: Vec<u64>,
    total: u64,
}

impl Backing {
    /// Backing over `entries`, in order.
    pub fn new(entries: Vec<MemEntry>) -> Self {
        let mut starts = Vec::with_capacity(entries.len());
        let mut total = 0u64;
        for entry in &entries {
            starts.push(total);
            total += u64::from(entry.len);
        }
        Self {
            entries,
            starts,
            total,
        }
    }

    /// Total bytes.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Entry count.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Index of the first entry whose first or last byte is not guest memory.
    pub fn first_unmapped(&self, mem: &GuestMemory) -> Option<usize> {
        let mut probe = [0u8; 1];
        self.entries.iter().position(|entry| {
            if entry.len == 0 {
                return false;
            }
            let last = entry.addr.checked_add(u64::from(entry.len) - 1);
            let Some(last) = last else {
                return true;
            };
            mem.read_bytes(entry.addr, &mut probe).is_err()
                || mem.read_bytes(last, &mut probe).is_err()
        })
    }

    /// Copy backing bytes `offset..offset + dst.len()` into `dst`.
    pub fn read(
        &self,
        mem: &GuestMemory,
        offset: u64,
        dst: &mut [u8],
    ) -> Result<(), TransferError> {
        let end = offset.checked_add(dst.len() as u64);
        let Some(end) = end.filter(|&end| end <= self.total) else {
            return Err(TransferError::PastBacking {
                start: offset,
                end: offset.saturating_add(dst.len() as u64),
                total: self.total,
            });
        };
        if dst.is_empty() {
            return Ok(());
        }
        // `offset < end <= total`, so at least entry 0 (start 0) qualifies.
        let mut index = self.starts.partition_point(|&start| start <= offset) - 1;
        let mut at = offset;
        let mut copied = 0usize;
        while at < end {
            let entry = self.entries[index];
            let within = at - self.starts[index];
            let take = (u64::from(entry.len) - within).min(end - at) as usize;
            if take > 0 {
                let addr = entry.addr + within;
                mem.read_bytes(addr, &mut dst[copied..copied + take])
                    .map_err(|_| TransferError::Unmapped { addr })?;
            }
            copied += take;
            at += take as u64;
            index += 1;
        }
        Ok(())
    }
}

/// One 2D resource.
#[derive(Debug)]
pub struct Resource {
    /// Pixel layout.
    pub format: PixelFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Host copy, `height` rows of `stride()` bytes.
    pub pixels: Vec<u8>,
    /// Guest pages, once attached.
    pub backing: Option<Backing>,
}

impl Resource {
    /// A zero-filled resource. The caller has checked the size against its budget.
    pub fn new(format: PixelFormat, width: u32, height: u32) -> Self {
        let bytes = u64::from(width) * u64::from(height) * u64::from(BYTES_PER_PIXEL);
        Self {
            format,
            width,
            height,
            pixels: vec![0; bytes as usize],
            backing: None,
        }
    }

    /// Bytes per row.
    pub fn stride(&self) -> usize {
        self.width as usize * BYTES_PER_PIXEL as usize
    }

    /// Copy `rect` (already checked to fit) from the backing into the host copy.
    /// Returns the bytes copied.
    pub fn transfer_from(
        &mut self,
        mem: &GuestMemory,
        backing: &Backing,
        rect: Rect,
        offset: u64,
    ) -> Result<u64, TransferError> {
        let stride = self.stride();
        let row_bytes = rect.width as usize * BYTES_PER_PIXEL as usize;
        for h in 0..rect.height as usize {
            let src =
                offset
                    .checked_add(stride as u64 * h as u64)
                    .ok_or(TransferError::PastBacking {
                        start: offset,
                        end: u64::MAX,
                        total: backing.total(),
                    })?;
            let dst = (rect.y as usize + h) * stride + rect.x as usize * BYTES_PER_PIXEL as usize;
            backing.read(mem, src, &mut self.pixels[dst..dst + row_bytes])?;
        }
        Ok(row_bytes as u64 * u64::from(rect.height))
    }
}

#[cfg(test)]
#[path = "resource_tests.rs"]
mod tests;
