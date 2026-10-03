//! Generic Address Structure (ACPI 6.5 §5.2.3.2): 12 bytes naming a register
//! block. Used by SPCR and DBG2 for the PL011.

use crate::AcpiError;

/// Bytes in a GAS.
pub const GAS_LEN: usize = 12;
/// Address space ID 0: system memory.
pub const SPACE_SYSTEM_MEMORY: u8 = 0;
/// Access size 3: 32-bit (dword) accesses.
pub const ACCESS_DWORD: u8 = 3;

/// One GAS, in field order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gas {
    /// Address space ID.
    pub space: u8,
    /// Register width in bits.
    pub bit_width: u8,
    /// Register bit offset.
    pub bit_offset: u8,
    /// Access size code (1 byte, 2 word, 3 dword, 4 qword).
    pub access_size: u8,
    /// Register address.
    pub address: u64,
}

impl Gas {
    /// 32-bit memory-mapped registers at `address` (an SBSA/PL011 UART).
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(address = %format!("{address:#x}")))]
    pub fn mmio32(address: u64) -> Self {
        Self {
            space: SPACE_SYSTEM_MEMORY,
            bit_width: 32,
            bit_offset: 0,
            access_size: ACCESS_DWORD,
            address,
        }
    }

    /// The 12 bytes.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
    pub fn encode(&self) -> [u8; GAS_LEN] {
        let mut out = [0u8; GAS_LEN];
        out[0] = self.space;
        out[1] = self.bit_width;
        out[2] = self.bit_offset;
        out[3] = self.access_size;
        out[4..].copy_from_slice(&self.address.to_le_bytes());
        out
    }

    /// Read a GAS from the first 12 bytes of `bytes`.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
    pub fn parse(bytes: &[u8]) -> Result<Self, AcpiError> {
        let raw = bytes.get(..GAS_LEN).ok_or(AcpiError::Truncated {
            what: "generic address structure",
            len: bytes.len(),
            need: GAS_LEN,
        })?;
        let mut address = [0u8; 8];
        address.copy_from_slice(&raw[4..12]);
        Ok(Self {
            space: raw[0],
            bit_width: raw[1],
            bit_offset: raw[2],
            access_size: raw[3],
            address: u64::from_le_bytes(address),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmio32_round_trips() {
        let gas = Gas::mmio32(0x0900_0000);
        let bytes = gas.encode();
        assert_eq!(&bytes[..4], &[0, 32, 0, 3]);
        assert_eq!(&bytes[4..], &0x0900_0000u64.to_le_bytes());
        assert_eq!(Gas::parse(&bytes), Ok(gas));
        assert!(Gas::parse(&bytes[..11]).is_err());
    }
}
