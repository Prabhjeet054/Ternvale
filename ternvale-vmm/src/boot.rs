//! Copy a raw AArch64 image into guest RAM and point the vCPU at it.
//!
//! The bare-metal test payload is linked at [`PAYLOAD_GPA`].

use sha2::{Digest, Sha256};

use crate::memory::{GuestMemory, MemoryError};
use crate::vcpu::{Vcpu, VcpuError};

/// Guest physical address where a raw payload is loaded.
pub const PAYLOAD_GPA: u64 = 0x4000_0000;

/// A raw image placed at [`PAYLOAD_GPA`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadInfo {
    /// Guest physical address of the first byte.
    pub gpa: u64,
    /// Image length in bytes.
    pub size: usize,
    /// SHA-256 of the image bytes.
    pub sha256: [u8; 32],
}

/// Loading a raw image failed.
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    /// The file or buffer had no bytes to copy.
    #[error("guest payload is empty")]
    Empty,
    /// The bytes did not fit in the region at [`PAYLOAD_GPA`].
    #[error("copy payload to {gpa:#x}: {source}")]
    Memory {
        /// Load address.
        gpa: u64,
        /// Guest memory error.
        source: MemoryError,
    },
    /// The vCPU rejected the PC write.
    #[error("set guest PC to {gpa:#x}: {source}")]
    Vcpu {
        /// Load address written to PC.
        gpa: u64,
        /// vCPU error.
        source: VcpuError,
    },
}

/// Copy `payload` to [`PAYLOAD_GPA`] and set `vcpu`'s PC there.
///
/// Logs the size, address, and SHA-256 on `ternvale::boot`. The region at
/// [`PAYLOAD_GPA`] must already be registered and large enough.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::boot",
    skip_all,
    fields(bytes = payload.len(), gpa = format!("{:#x}", PAYLOAD_GPA))
)]
pub fn load(memory: &mut GuestMemory, vcpu: &Vcpu, payload: &[u8]) -> Result<LoadInfo, BootError> {
    let info = stage(memory, payload)?;
    vcpu.set_pc(info.gpa).map_err(|source| {
        tracing::error!(
            target: "ternvale::boot",
            gpa = format!("{:#x}", info.gpa),
            error = %source,
            "failed to set guest PC"
        );
        BootError::Vcpu {
            gpa: info.gpa,
            source,
        }
    })?;
    tracing::info!(
        target: "ternvale::boot",
        pc = format!("{:#x}", info.gpa),
        "set guest PC"
    );
    Ok(info)
}

/// Copy `payload` to [`PAYLOAD_GPA`] without touching a vCPU.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::boot",
    skip_all,
    fields(bytes = payload.len(), gpa = format!("{:#x}", PAYLOAD_GPA))
)]
pub fn stage(memory: &mut GuestMemory, payload: &[u8]) -> Result<LoadInfo, BootError> {
    if payload.is_empty() {
        tracing::warn!(target: "ternvale::boot", "rejected empty guest payload");
        return Err(BootError::Empty);
    }
    let sha256 = sha256(payload);
    memory
        .write_bytes(PAYLOAD_GPA, payload)
        .map_err(|source| BootError::Memory {
            gpa: PAYLOAD_GPA,
            source,
        })?;
    tracing::info!(
        target: "ternvale::boot",
        size = payload.len(),
        address = format!("{:#x}", PAYLOAD_GPA),
        sha256 = %hex(&sha256),
        "loaded guest payload"
    );
    Ok(LoadInfo {
        gpa: PAYLOAD_GPA,
        size: payload.len(),
        sha256,
    })
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn hex(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0xf)] as char);
    }
    out
}

#[cfg(test)]
#[path = "boot_hv_test.rs"]
mod hv_test;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::HOST_PAGE_SIZE;

    fn memory() -> GuestMemory {
        let mut memory = GuestMemory::new().expect("host page size");
        memory
            .add_region(PAYLOAD_GPA, HOST_PAGE_SIZE)
            .expect("region");
        memory
    }

    #[test]
    fn stages_bytes_and_logs_size_address_and_sha256() {
        let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-boot", std::process::id()));
        std::fs::create_dir_all(&dir).expect("log dir");
        let guard =
            ternvale_log::init(ternvale_log::LogConfig::new("boot", dir.clone())).expect("log");
        let payload = b"Hello from Ternvale\n";
        let mut memory = memory();
        let info = stage(&mut memory, payload).expect("stage");
        assert_eq!(info.gpa, PAYLOAD_GPA);
        assert_eq!(info.size, payload.len());
        let mut read = vec![0u8; payload.len()];
        memory
            .read_bytes(PAYLOAD_GPA, &mut read)
            .expect("read back");
        assert_eq!(read, payload);
        assert_eq!(info.sha256, sha256(payload));
        let path = guard.log_path().to_path_buf();
        drop(guard);
        let text = std::fs::read_to_string(&path).expect("log");
        assert!(text.contains("loaded guest payload"), "{text}");
        assert!(text.contains("size=20"), "{text}");
        assert!(text.contains("address=\"0x40000000\""), "{text}");
        assert!(text.contains(&hex(&info.sha256)), "{text}");
        std::fs::remove_dir_all(&dir).expect("remove log dir");
    }

    #[test]
    fn rejects_an_empty_payload() {
        let mut memory = memory();
        let error = stage(&mut memory, b"").unwrap_err();
        assert!(matches!(error, BootError::Empty), "{error}");
        assert_eq!(memory.read_u8(PAYLOAD_GPA).expect("untouched"), 0);
    }

    #[test]
    fn rejects_a_payload_past_the_region() {
        let mut memory = memory();
        let payload = vec![0x11u8; HOST_PAGE_SIZE as usize + 1];
        let error = stage(&mut memory, &payload).unwrap_err();
        assert!(
            matches!(
                error,
                BootError::Memory {
                    gpa: PAYLOAD_GPA,
                    ..
                }
            ),
            "{error}"
        );
    }
}
