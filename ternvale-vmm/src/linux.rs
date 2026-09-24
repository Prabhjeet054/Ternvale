//! Load a Linux arm64 `Image`, initramfs, and DTB, then set the boot registers.
//!
//! Placement follows `Documentation/arch/arm64/booting.rst`. See `docs/BOOT.md`.

use crate::memory::{GuestMemory, MemoryError};
use crate::vcpu::{Vcpu, VcpuError};

/// Little-endian magic at header offset 56: `ARM\x64`.
pub const IMAGE_MAGIC: u32 = 0x644d_5241;
/// Bytes in the Image header.
pub const HEADER_LEN: usize = 64;
/// Offset of `text_offset`.
const TEXT_OFFSET_AT: usize = 8;
/// Offset of `image_size`.
const IMAGE_SIZE_AT: usize = 16;
/// Offset of `flags`.
const FLAGS_AT: usize = 24;
/// Offset of the magic.
const MAGIC_AT: usize = 56;
/// Kernel must sit this far from a 2 MiB-aligned base.
pub const KERNEL_ALIGN: u64 = 2 * 1024 * 1024;
/// DTB must be at most this long.
pub const DTB_MAX: u64 = 2 * 1024 * 1024;
/// Initrd must lie in a 1 GiB-aligned window no larger than this.
const INITRD_WINDOW_MAX: u64 = 32 * 1024 * 1024 * 1024;
/// `PSTATE`: EL1h (`M = 0b0101`) with DAIF masked (`0x3c0`).
pub const CPSR_EL1H_MASKED: u64 = 0x3c5;
/// `flags` bit 0: kernel is big-endian.
const FLAG_BE: u64 = 1;

/// Parsed Image header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageHeader {
    /// Bytes from a 2 MiB-aligned base to the first Image byte.
    pub text_offset: u64,
    /// Bytes the kernel needs from the start of the Image, including BSS.
    pub image_size: u64,
    /// Little-endian kernel flags.
    pub flags: u64,
}

/// Guest physical addresses chosen for one boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxLayout {
    /// First byte of the Image. This is also the kernel entry.
    pub kernel: u64,
    /// Bytes reserved for the kernel (`image_size`, or the file if it is larger).
    pub kernel_bytes: u64,
    /// First byte of the initramfs. Zero when none was supplied.
    pub initrd: u64,
    /// Initramfs length. Zero when none was supplied.
    pub initrd_bytes: u64,
    /// First byte of the DTB. Written into `x0`.
    pub dtb: u64,
    /// DTB length.
    pub dtb_bytes: u64,
}

/// Registers the primary CPU must hold before the first kernel instruction.
pub trait BootRegs {
    /// Write `Xn`.
    fn set_gpr(&self, index: u8, value: u64) -> Result<(), LinuxBootError>;
    /// Write the program counter.
    fn set_pc(&self, value: u64) -> Result<(), LinuxBootError>;
    /// Write `CPSR`.
    fn set_cpsr(&self, value: u64) -> Result<(), LinuxBootError>;
}

impl BootRegs for Vcpu {
    fn set_gpr(&self, index: u8, value: u64) -> Result<(), LinuxBootError> {
        self.set_x(index, value).map_err(LinuxBootError::from)
    }

    fn set_pc(&self, value: u64) -> Result<(), LinuxBootError> {
        Vcpu::set_pc(self, value).map_err(LinuxBootError::from)
    }

    fn set_cpsr(&self, value: u64) -> Result<(), LinuxBootError> {
        Vcpu::set_cpsr(self, value).map_err(LinuxBootError::from)
    }
}

/// Loading a Linux Image failed.
#[derive(Debug, thiserror::Error)]
pub enum LinuxBootError {
    /// The buffer is shorter than the 64-byte header.
    #[error("linux image is {len} bytes, header needs {HEADER_LEN}")]
    Truncated {
        /// Bytes supplied.
        len: usize,
    },
    /// Magic at offset 56 was not `ARM\x64`.
    #[error("linux image magic {magic:#x} is not {IMAGE_MAGIC:#x}")]
    BadMagic {
        /// Value read at offset 56.
        magic: u32,
    },
    /// `image_size` is zero, so the reservation is unbounded.
    #[error("linux image_size is zero")]
    ZeroImageSize,
    /// `flags` bit 0 is set. Ternvale loads little-endian Images only.
    #[error("linux image is big-endian")]
    BigEndian,
    /// The DTB is missing or longer than 2 MiB.
    #[error("dtb size {size:#x} is empty or above {DTB_MAX:#x}")]
    DtbSize {
        /// DTB length.
        size: u64,
    },
    /// Kernel, initrd, or DTB does not fit in RAM without overlap.
    #[error("linux boot does not fit in ram {ram_base:#x} size {ram_size:#x}: {reason}")]
    NoFit {
        /// First byte of guest RAM.
        ram_base: u64,
        /// Guest RAM length.
        ram_size: u64,
        /// Which placement failed.
        reason: &'static str,
    },
    /// A copy into guest RAM failed.
    #[error("copy linux {what} to {gpa:#x}: {source}")]
    Memory {
        /// `kernel`, `initrd`, or `dtb`.
        what: &'static str,
        /// Destination GPA.
        gpa: u64,
        /// Guest memory error.
        source: MemoryError,
    },
    /// A boot register write failed.
    #[error("set linux boot register: {0}")]
    Vcpu(#[from] VcpuError),
}

/// Read `text_offset`, `image_size`, and `flags`. Reject a bad magic.
#[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(bytes = image.len()))]
pub fn parse_header(image: &[u8]) -> Result<ImageHeader, LinuxBootError> {
    if image.len() < HEADER_LEN {
        tracing::warn!(target: "ternvale::boot", len = image.len(), "rejected short linux image");
        return Err(LinuxBootError::Truncated { len: image.len() });
    }
    let magic = read_u32(image, MAGIC_AT);
    if magic != IMAGE_MAGIC {
        tracing::warn!(
            target: "ternvale::boot",
            magic = format!("{:#x}", magic),
            "rejected linux image magic"
        );
        return Err(LinuxBootError::BadMagic { magic });
    }
    let header = ImageHeader {
        text_offset: u64_at(image, TEXT_OFFSET_AT),
        image_size: u64_at(image, IMAGE_SIZE_AT),
        flags: u64_at(image, FLAGS_AT),
    };
    if header.image_size == 0 {
        tracing::warn!(target: "ternvale::boot", "rejected linux image with image_size 0");
        return Err(LinuxBootError::ZeroImageSize);
    }
    if header.flags & FLAG_BE != 0 {
        tracing::warn!(target: "ternvale::boot", flags = format!("{:#x}", header.flags), "rejected big-endian linux image");
        return Err(LinuxBootError::BigEndian);
    }
    tracing::debug!(
        target: "ternvale::boot",
        text_offset = format!("{:#x}", header.text_offset),
        image_size = format!("{:#x}", header.image_size),
        flags = format!("{:#x}", header.flags),
        "parsed linux image header"
    );
    Ok(header)
}

/// Choose non-overlapping GPAs inside `ram_base` / `ram_size`.
///
/// The kernel is `text_offset` bytes above the 2 MiB boundary at or above
/// `ram_base`. The DTB follows the kernel, 8-byte aligned. The initramfs
/// follows the DTB. An empty `initrd_len` omits the initramfs.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::boot",
    skip_all,
    fields(
        ram_base = format!("{:#x}", ram_base),
        ram_size = format!("{:#x}", ram_size),
        text_offset = format!("{:#x}", header.text_offset),
    )
)]
pub fn place(
    ram_base: u64,
    ram_size: u64,
    header: &ImageHeader,
    kernel_len: u64,
    initrd_len: u64,
    dtb_len: u64,
) -> Result<LinuxLayout, LinuxBootError> {
    if dtb_len == 0 || dtb_len > DTB_MAX {
        tracing::warn!(target: "ternvale::boot", size = format!("{:#x}", dtb_len), "rejected dtb size");
        return Err(LinuxBootError::DtbSize { size: dtb_len });
    }
    let ram_end = ram_base
        .checked_add(ram_size)
        .ok_or_else(|| no_fit(ram_base, ram_size, "ram end overflow"))?;
    let aligned = align_up(ram_base, KERNEL_ALIGN)
        .ok_or_else(|| no_fit(ram_base, ram_size, "kernel align overflow"))?;
    let kernel = aligned
        .checked_add(header.text_offset)
        .ok_or_else(|| no_fit(ram_base, ram_size, "text_offset overflow"))?;
    let kernel_bytes = header.image_size.max(kernel_len);
    let kernel_end = kernel
        .checked_add(kernel_bytes)
        .ok_or_else(|| no_fit(ram_base, ram_size, "kernel end overflow"))?;
    if kernel < ram_base || kernel_end > ram_end {
        return Err(no_fit(ram_base, ram_size, "kernel does not fit"));
    }
    let dtb =
        align_up(kernel_end, 8).ok_or_else(|| no_fit(ram_base, ram_size, "dtb align overflow"))?;
    let dtb_end = dtb
        .checked_add(dtb_len)
        .ok_or_else(|| no_fit(ram_base, ram_size, "dtb end overflow"))?;
    if dtb_end > ram_end {
        return Err(no_fit(ram_base, ram_size, "dtb does not fit"));
    }
    let (initrd, initrd_end) = if initrd_len == 0 {
        (0, dtb_end)
    } else {
        let initrd = align_up(dtb_end, 0x1000)
            .ok_or_else(|| no_fit(ram_base, ram_size, "initrd align overflow"))?;
        let initrd_end = initrd
            .checked_add(initrd_len)
            .ok_or_else(|| no_fit(ram_base, ram_size, "initrd end overflow"))?;
        let window = kernel & !(1024 * 1024 * 1024 - 1);
        let window_end = window.saturating_add(INITRD_WINDOW_MAX);
        if initrd_end > ram_end || initrd < window || initrd_end > window_end {
            return Err(no_fit(ram_base, ram_size, "initrd does not fit"));
        }
        (initrd, initrd_end)
    };
    let layout = LinuxLayout {
        kernel,
        kernel_bytes,
        initrd,
        initrd_bytes: initrd_len,
        dtb,
        dtb_bytes: dtb_len,
    };
    tracing::info!(
        target: "ternvale::boot",
        kernel = format!("{:#x}", layout.kernel),
        kernel_bytes = format!("{:#x}", layout.kernel_bytes),
        initrd = format!("{:#x}", layout.initrd),
        initrd_bytes = format!("{:#x}", layout.initrd_bytes),
        dtb = format!("{:#x}", layout.dtb),
        dtb_bytes = format!("{:#x}", layout.dtb_bytes),
        ram_end = format!("{:#x}", ram_end),
        placed_end = format!("{:#x}", initrd_end),
        "placed linux kernel initrd and dtb"
    );
    Ok(layout)
}

/// Copy the Image, initramfs, and DTB, then set `x0`–`x3`, `PC`, and `CPSR`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::boot",
    skip_all,
    fields(kernel_bytes = kernel.len(), initrd_bytes = initrd.len(), dtb_bytes = dtb.len())
)]
pub fn load_linux(
    memory: &mut GuestMemory,
    cpu: &impl BootRegs,
    ram_base: u64,
    ram_size: u64,
    kernel: &[u8],
    initrd: &[u8],
    dtb: &[u8],
) -> Result<LinuxLayout, LinuxBootError> {
    let header = parse_header(kernel)?;
    let layout = place(
        ram_base,
        ram_size,
        &header,
        kernel.len() as u64,
        initrd.len() as u64,
        dtb.len() as u64,
    )?;
    copy(memory, "kernel", layout.kernel, kernel)?;
    if !initrd.is_empty() {
        copy(memory, "initrd", layout.initrd, initrd)?;
    }
    copy(memory, "dtb", layout.dtb, dtb)?;
    cpu.set_gpr(0, layout.dtb)?;
    cpu.set_gpr(1, 0)?;
    cpu.set_gpr(2, 0)?;
    cpu.set_gpr(3, 0)?;
    cpu.set_pc(layout.kernel)?;
    cpu.set_cpsr(CPSR_EL1H_MASKED)?;
    tracing::info!(
        target: "ternvale::boot",
        x0 = format!("{:#x}", layout.dtb),
        pc = format!("{:#x}", layout.kernel),
        cpsr = format!("{:#x}", CPSR_EL1H_MASKED),
        "set linux boot registers"
    );
    Ok(layout)
}

fn copy(
    memory: &mut GuestMemory,
    what: &'static str,
    gpa: u64,
    bytes: &[u8],
) -> Result<(), LinuxBootError> {
    memory.write_bytes(gpa, bytes).map_err(|source| {
        tracing::error!(
            target: "ternvale::boot",
            what,
            gpa = format!("{:#x}", gpa),
            error = %source,
            "failed to copy linux image"
        );
        LinuxBootError::Memory { what, gpa, source }
    })
}

fn read_u32(image: &[u8], offset: usize) -> u32 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&image[offset..offset + 4]);
    u32::from_le_bytes(buf)
}

fn u64_at(image: &[u8], offset: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&image[offset..offset + 8]);
    u64::from_le_bytes(buf)
}

fn align_up(value: u64, align: u64) -> Option<u64> {
    let mask = align - 1;
    if value & mask == 0 {
        Some(value)
    } else {
        value.checked_add(align - (value & mask))
    }
}

fn no_fit(ram_base: u64, ram_size: u64, reason: &'static str) -> LinuxBootError {
    tracing::warn!(target: "ternvale::boot", reason, "rejected linux placement");
    LinuxBootError::NoFit {
        ram_base,
        ram_size,
        reason,
    }
}

#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;
