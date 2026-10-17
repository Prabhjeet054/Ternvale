//! virtio-gpu wire format: the control header, 2D commands and responses.
//!
//! Layouts are from virtio 1.2 §5.7.6.7 ("Device Operation: Request header")
//! and §5.7.6.8 ("Device Operation: controlq"); every field is little-endian.
//! They match Linux `include/uapi/linux/virtio_gpu.h`. TODO(verify): the
//! section numbers against the issue of the spec you cite.

use std::fmt;

/// `VIRTIO_GPU_CMD_GET_DISPLAY_INFO`.
pub const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
/// `VIRTIO_GPU_CMD_RESOURCE_CREATE_2D`.
pub const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
/// `VIRTIO_GPU_CMD_RESOURCE_UNREF`.
pub const CMD_RESOURCE_UNREF: u32 = 0x0102;
/// `VIRTIO_GPU_CMD_SET_SCANOUT`.
pub const CMD_SET_SCANOUT: u32 = 0x0103;
/// `VIRTIO_GPU_CMD_RESOURCE_FLUSH`.
pub const CMD_RESOURCE_FLUSH: u32 = 0x0104;
/// `VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D`.
pub const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
/// `VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING`.
pub const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
/// `VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING`.
pub const CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
/// `VIRTIO_GPU_CMD_UPDATE_CURSOR` (cursorq).
pub const CMD_UPDATE_CURSOR: u32 = 0x0300;
/// `VIRTIO_GPU_CMD_MOVE_CURSOR` (cursorq).
pub const CMD_MOVE_CURSOR: u32 = 0x0301;

/// `VIRTIO_GPU_RESP_OK_NODATA`.
pub const RESP_OK_NODATA: u32 = 0x1100;
/// `VIRTIO_GPU_RESP_OK_DISPLAY_INFO`.
pub const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
/// `VIRTIO_GPU_RESP_ERR_UNSPEC`.
pub const RESP_ERR_UNSPEC: u32 = 0x1200;
/// `VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY`.
pub const RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
/// `VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID`.
pub const RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
/// `VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID`.
pub const RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
/// `VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER`.
pub const RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;

/// `VIRTIO_GPU_FLAG_FENCE`: the response must echo the fence.
pub const FLAG_FENCE: u32 = 1 << 0;
/// `VIRTIO_GPU_MAX_SCANOUTS`.
pub const MAX_SCANOUTS: usize = 16;
/// `sizeof(struct virtio_gpu_ctrl_hdr)`.
pub const HDR_LEN: usize = 24;
/// `sizeof(struct virtio_gpu_mem_entry)`.
pub const MEM_ENTRY_LEN: usize = 16;
/// `sizeof(struct virtio_gpu_resp_display_info)`: header plus 16 × 24-byte modes.
pub const DISPLAY_INFO_LEN: usize = HDR_LEN + MAX_SCANOUTS * 24;

/// `struct virtio_gpu_ctrl_hdr`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CtrlHdr {
    /// Command or response type.
    pub kind: u32,
    /// `VIRTIO_GPU_FLAG_*`.
    pub flags: u32,
    /// Fence to signal when `FLAG_FENCE` is set.
    pub fence_id: u64,
    /// 3D context (always 0 in 2D).
    pub ctx_id: u32,
    /// Fence ring (only with `VIRTIO_GPU_F_CONTEXT_INIT`, never offered).
    pub ring_idx: u8,
}

/// `struct virtio_gpu_rect`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left edge in pixels.
    pub x: u32,
    /// Top edge in pixels.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Rect {
    /// `true` when the rectangle is non-empty and inside a `width` × `height` image.
    /// Guest values: the sums are checked so they cannot wrap.
    pub fn fits_in(&self, width: u32, height: u32) -> bool {
        let right = self.x.checked_add(self.width);
        let bottom = self.y.checked_add(self.height);
        self.width > 0
            && self.height > 0
            && right.is_some_and(|r| r <= width)
            && bottom.is_some_and(|b| b <= height)
    }

    /// Overlap of two rectangles, if any.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (u64::from(self.x) + u64::from(self.width))
            .min(u64::from(other.x) + u64::from(other.width));
        let y1 = (u64::from(self.y) + u64::from(self.height))
            .min(u64::from(other.y) + u64::from(other.height));
        (x1 > u64::from(x0) && y1 > u64::from(y0)).then(|| Rect {
            x: x0,
            y: y0,
            width: (x1 - u64::from(x0)) as u32,
            height: (y1 - u64::from(y0)) as u32,
        })
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}+{}+{}", self.width, self.height, self.x, self.y)
    }
}

/// `struct virtio_gpu_mem_entry`: one guest-physical span of a resource's backing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemEntry {
    /// Guest physical address.
    pub addr: u64,
    /// Length in bytes.
    pub len: u32,
}

/// A parsed control-queue command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `GET_DISPLAY_INFO`.
    GetDisplayInfo,
    /// `RESOURCE_CREATE_2D`.
    ResourceCreate2d {
        /// Guest-chosen resource id (0 is invalid).
        resource_id: u32,
        /// `VIRTIO_GPU_FORMAT_*`.
        format: u32,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
    /// `RESOURCE_UNREF`.
    ResourceUnref {
        /// Resource to destroy.
        resource_id: u32,
    },
    /// `SET_SCANOUT`. `resource_id` 0 disables the scanout.
    SetScanout {
        /// Part of the resource shown.
        rect: Rect,
        /// Display output.
        scanout_id: u32,
        /// Resource to show.
        resource_id: u32,
    },
    /// `RESOURCE_FLUSH`.
    ResourceFlush {
        /// Region of the resource that changed.
        rect: Rect,
        /// Resource to flush.
        resource_id: u32,
    },
    /// `TRANSFER_TO_HOST_2D`.
    TransferToHost2d {
        /// Region of the resource to update.
        rect: Rect,
        /// Byte offset of the region's first pixel in the backing.
        offset: u64,
        /// Target resource.
        resource_id: u32,
    },
    /// `RESOURCE_ATTACH_BACKING`.
    ResourceAttachBacking {
        /// Target resource.
        resource_id: u32,
        /// Guest pages, in order.
        entries: Vec<MemEntry>,
    },
    /// `RESOURCE_DETACH_BACKING`.
    ResourceDetachBacking {
        /// Target resource.
        resource_id: u32,
    },
    /// A type this 2D device does not implement (3D, capsets, EDID, blobs, cursor).
    Unsupported,
}

/// Why a request could not be parsed. Answered with `RESP_ERR_UNSPEC`
/// (or `RESP_ERR_INVALID_PARAMETER` for a bad entry count).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Shorter than the header.
    #[error("request is {len} bytes, shorter than the {HDR_LEN}-byte header")]
    NoHeader {
        /// Bytes available.
        len: usize,
    },
    /// Shorter than its command's fixed body.
    #[error("{name} needs {need} bytes, request has {len}")]
    Short {
        /// Command name.
        name: &'static str,
        /// Bytes required.
        need: usize,
        /// Bytes available.
        len: usize,
    },
    /// `nr_entries` above the cap.
    #[error("attach backing lists {entries} entries, more than {max}")]
    TooManyEntries {
        /// Count the guest sent.
        entries: u32,
        /// Cap.
        max: u32,
    },
}

/// Most backing entries one `RESOURCE_ATTACH_BACKING` may list (QEMU's cap).
pub const MAX_BACKING_ENTRIES: u32 = 16 * 1024;

/// Human name of a command or response type, for logs.
pub fn type_name(kind: u32) -> &'static str {
    match kind {
        CMD_GET_DISPLAY_INFO => "GET_DISPLAY_INFO",
        CMD_RESOURCE_CREATE_2D => "RESOURCE_CREATE_2D",
        CMD_RESOURCE_UNREF => "RESOURCE_UNREF",
        CMD_SET_SCANOUT => "SET_SCANOUT",
        CMD_RESOURCE_FLUSH => "RESOURCE_FLUSH",
        CMD_TRANSFER_TO_HOST_2D => "TRANSFER_TO_HOST_2D",
        CMD_RESOURCE_ATTACH_BACKING => "RESOURCE_ATTACH_BACKING",
        CMD_RESOURCE_DETACH_BACKING => "RESOURCE_DETACH_BACKING",
        0x0108 => "GET_CAPSET_INFO",
        0x0109 => "GET_CAPSET",
        0x010a => "GET_EDID",
        0x010b => "RESOURCE_ASSIGN_UUID",
        0x010c => "RESOURCE_CREATE_BLOB",
        0x010d => "SET_SCANOUT_BLOB",
        0x0200..=0x02ff => "3D command",
        CMD_UPDATE_CURSOR => "UPDATE_CURSOR",
        CMD_MOVE_CURSOR => "MOVE_CURSOR",
        RESP_OK_NODATA => "OK_NODATA",
        RESP_OK_DISPLAY_INFO => "OK_DISPLAY_INFO",
        RESP_ERR_UNSPEC => "ERR_UNSPEC",
        RESP_ERR_OUT_OF_MEMORY => "ERR_OUT_OF_MEMORY",
        RESP_ERR_INVALID_SCANOUT_ID => "ERR_INVALID_SCANOUT_ID",
        RESP_ERR_INVALID_RESOURCE_ID => "ERR_INVALID_RESOURCE_ID",
        RESP_ERR_INVALID_PARAMETER => "ERR_INVALID_PARAMETER",
        _ => "unknown",
    }
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

fn rect(bytes: &[u8], at: usize) -> Rect {
    Rect {
        x: le32(bytes, at),
        y: le32(bytes, at + 4),
        width: le32(bytes, at + 8),
        height: le32(bytes, at + 12),
    }
}

/// Parse the header at the start of `bytes`.
pub fn parse_hdr(bytes: &[u8]) -> Result<CtrlHdr, ParseError> {
    if bytes.len() < HDR_LEN {
        return Err(ParseError::NoHeader { len: bytes.len() });
    }
    Ok(CtrlHdr {
        kind: le32(bytes, 0),
        flags: le32(bytes, 4),
        fence_id: le64(bytes, 8),
        ctx_id: le32(bytes, 16),
        ring_idx: bytes[20],
    })
}

/// Parse one control-queue request: the header and its command body.
pub fn parse(bytes: &[u8]) -> Result<(CtrlHdr, Command), ParseError> {
    let hdr = parse_hdr(bytes)?;
    let body = &bytes[HDR_LEN..];
    let need = |n: usize| {
        if body.len() < n {
            Err(ParseError::Short {
                name: type_name(hdr.kind),
                need: HDR_LEN + n,
                len: bytes.len(),
            })
        } else {
            Ok(())
        }
    };
    let command = match hdr.kind {
        CMD_GET_DISPLAY_INFO => Command::GetDisplayInfo,
        CMD_RESOURCE_CREATE_2D => {
            need(16)?;
            Command::ResourceCreate2d {
                resource_id: le32(body, 0),
                format: le32(body, 4),
                width: le32(body, 8),
                height: le32(body, 12),
            }
        }
        CMD_RESOURCE_UNREF => {
            need(8)?;
            Command::ResourceUnref {
                resource_id: le32(body, 0),
            }
        }
        CMD_SET_SCANOUT => {
            need(24)?;
            Command::SetScanout {
                rect: rect(body, 0),
                scanout_id: le32(body, 16),
                resource_id: le32(body, 20),
            }
        }
        CMD_RESOURCE_FLUSH => {
            need(24)?;
            Command::ResourceFlush {
                rect: rect(body, 0),
                resource_id: le32(body, 16),
            }
        }
        CMD_TRANSFER_TO_HOST_2D => {
            need(32)?;
            Command::TransferToHost2d {
                rect: rect(body, 0),
                offset: le64(body, 16),
                resource_id: le32(body, 24),
            }
        }
        CMD_RESOURCE_ATTACH_BACKING => {
            need(8)?;
            let resource_id = le32(body, 0);
            let count = le32(body, 4);
            if count > MAX_BACKING_ENTRIES {
                return Err(ParseError::TooManyEntries {
                    entries: count,
                    max: MAX_BACKING_ENTRIES,
                });
            }
            need(8 + count as usize * MEM_ENTRY_LEN)?;
            let entries = (0..count as usize)
                .map(|i| {
                    let at = 8 + i * MEM_ENTRY_LEN;
                    MemEntry {
                        addr: le64(body, at),
                        len: le32(body, at + 8),
                    }
                })
                .collect();
            Command::ResourceAttachBacking {
                resource_id,
                entries,
            }
        }
        CMD_RESOURCE_DETACH_BACKING => {
            need(8)?;
            Command::ResourceDetachBacking {
                resource_id: le32(body, 0),
            }
        }
        _ => Command::Unsupported,
    };
    Ok((hdr, command))
}

/// The response header for `request`: `kind`, with the fence echoed when the
/// request asked for one (§5.7.6.7: a fenced request's response carries the
/// same `fence_id` and `ctx_id`).
pub fn response_hdr(request: &CtrlHdr, kind: u32) -> [u8; HDR_LEN] {
    let mut out = [0u8; HDR_LEN];
    out[0..4].copy_from_slice(&kind.to_le_bytes());
    if request.flags & FLAG_FENCE != 0 {
        out[4..8].copy_from_slice(&FLAG_FENCE.to_le_bytes());
        out[8..16].copy_from_slice(&request.fence_id.to_le_bytes());
        out[16..20].copy_from_slice(&request.ctx_id.to_le_bytes());
    }
    out
}

/// `struct virtio_gpu_resp_display_info` for scanouts of `modes` (width, height);
/// every listed scanout is enabled at origin 0,0.
pub fn display_info(request: &CtrlHdr, modes: &[(u32, u32)]) -> Vec<u8> {
    let mut out = vec![0u8; DISPLAY_INFO_LEN];
    out[..HDR_LEN].copy_from_slice(&response_hdr(request, RESP_OK_DISPLAY_INFO));
    for (i, (width, height)) in modes.iter().take(MAX_SCANOUTS).enumerate() {
        let at = HDR_LEN + i * 24;
        out[at + 8..at + 12].copy_from_slice(&width.to_le_bytes());
        out[at + 12..at + 16].copy_from_slice(&height.to_le_bytes());
        out[at + 16..at + 20].copy_from_slice(&1u32.to_le_bytes());
    }
    out
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
