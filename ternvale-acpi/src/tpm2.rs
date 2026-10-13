//! The TPM 2.0 ACPI table and the DSDT `\_SB.TPM2` device.
//!
//! TCG ACPI Specification (Family "1.2" and "2.0"), Version 1.3, §8 "TPM2
//! ACPI Table", Table 7 (TODO(verify) the section and table numbers in the
//! revision you check against; the layout matches QEMU's `build_tpm2` and
//! Linux's `struct acpi_table_tpm2` + `struct acpi_tpm2_phy`). Revision 4,
//! 76 bytes:
//!
//! | Offset | Field | Ternvale |
//! | --- | --- | --- |
//! | 36 | Platform Class (u16) | 0, client |
//! | 38 | Reserved (u16) | 0 |
//! | 40 | Address of CRB Control Area (u64) | CRB base + `0x40` |
//! | 48 | Start Method (u32) | 7, Command Response Buffer |
//! | 52 | Start Method Specific Parameters (12 bytes) | 0 (none for CRB) |
//! | 64 | Log Area Minimum Length, LAML (u32) | 0 |
//! | 68 | Log Area Start Address, LASA (u64) | 0 |
//!
//! There is no TCG event log yet, so LAML and LASA are 0 (Linux then skips the
//! ACPI log). TODO(verify): whether Windows needs a non-zero LAML/LASA.
//! Start method 7 means the OS writes `TPM_CRB_CTRL_START` itself, with no
//! ACPI `_DSM` start (method 8) or Arm SMC (method 11). Linux requires the
//! control area to sit 0x40 bytes into the first `_CRS` range.
//!
//! The device (the TCG ACPI Specification's TPM 2.0 device object section,
//! TODO(verify) its number; Microsoft documents the same `_HID`):
//! `Device (TPM2)` under `\_SB` with `_HID "MSFT0101"`,
//! `_UID 0`, and `_CRS` one `Memory32Fixed (ReadWrite)` over the whole CRB
//! window (five locality pages). QEMU's Arm `virt` names it `TPM0` with the
//! same `_HID`; the name itself is not significant to OSes.
//! TODO(verify): Windows' acceptance of start method 7 on Arm64 with this
//! device (no `_STR`, `_STA` or `_DSM`; absent `_STA` means present).

use crate::aml::{device, name};
use crate::aml_data::{integer, string};
use crate::aml_resource::{memory32_fixed, resource_template};
use crate::config::TpmConfig;
use crate::sdt::{table, SdtHeader, SDT_HEADER_LEN};
use crate::AcpiError;

/// TPM2 signature.
pub const TPM2_SIGNATURE: [u8; 4] = *b"TPM2";
/// TPM2 revision with LAML/LASA.
pub const TPM2_REVISION: u8 = 4;
/// Whole table.
pub const TPM2_LEN: usize = 76;
/// Platform Class: client.
pub const PLATFORM_CLIENT: u16 = 0;
/// Start Method: Command Response Buffer.
pub const START_METHOD_CRB: u32 = 7;
/// `_HID` of a TPM 2.0 device.
pub const TPM_HID: &str = "MSFT0101";
/// Offset of the control area address in the table.
const CONTROL_AREA_OFFSET: usize = 40;
/// Offset of the start method.
const START_METHOD_OFFSET: usize = 48;

/// What the TPM2 table says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tpm2Info {
    /// Platform Class.
    pub platform_class: u16,
    /// Address of CRB Control Area.
    pub control_area: u64,
    /// Start Method.
    pub start_method: u32,
}

/// The TPM2 table for `tpm`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(control_area = %format!("{:#x}", tpm.control_area)))]
pub fn tpm2(tpm: &TpmConfig) -> Result<Vec<u8>, AcpiError> {
    let mut body = Vec::with_capacity(TPM2_LEN - SDT_HEADER_LEN);
    body.extend_from_slice(&PLATFORM_CLIENT.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body.extend_from_slice(&tpm.control_area.to_le_bytes());
    body.extend_from_slice(&START_METHOD_CRB.to_le_bytes());
    body.extend_from_slice(&[0; 12]);
    body.extend_from_slice(&0u32.to_le_bytes());
    body.extend_from_slice(&0u64.to_le_bytes());
    tracing::debug!(
        target: "ternvale::acpi",
        control_area = %format!("{:#x}", tpm.control_area),
        start_method = START_METHOD_CRB,
        "tpm2 table built"
    );
    table(SdtHeader::new(TPM2_SIGNATURE, TPM2_REVISION), &body)
}

/// Decode a `TPM2`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(len = bytes.len()))]
pub fn decode_tpm2(bytes: &[u8]) -> Result<Tpm2Info, AcpiError> {
    let header = SdtHeader::parse(bytes)?;
    let bad = |reason: String| {
        tracing::warn!(target: "ternvale::acpi", reason = %reason, "tpm2 table malformed");
        AcpiError::BadTable {
            what: "TPM2".to_string(),
            gpa: 0,
            reason,
        }
    };
    if header.signature != TPM2_SIGNATURE {
        return Err(bad(format!(
            "signature {:?}",
            String::from_utf8_lossy(&header.signature)
        )));
    }
    let len = header.length as usize;
    if len < START_METHOD_OFFSET + 4 || len > bytes.len() {
        return Err(bad(format!("length {len} with {} bytes", bytes.len())));
    }
    let u64_at = |at: usize| {
        let mut word = [0; 8];
        word.copy_from_slice(&bytes[at..at + 8]);
        u64::from_le_bytes(word)
    };
    Ok(Tpm2Info {
        platform_class: u16::from_le_bytes([bytes[36], bytes[37]]),
        control_area: u64_at(CONTROL_AREA_OFFSET),
        start_method: u32::from_le_bytes([
            bytes[START_METHOD_OFFSET],
            bytes[START_METHOD_OFFSET + 1],
            bytes[START_METHOD_OFFSET + 2],
            bytes[START_METHOD_OFFSET + 3],
        ]),
    })
}

/// `Device (TPM2) { _HID "MSFT0101", _UID 0, _CRS Memory32Fixed }`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(crb = %format!("{:#x}+{:#x}", tpm.crb_base, tpm.crb_len))
)]
pub fn tpm_device(tpm: &TpmConfig) -> Result<Vec<u8>, AcpiError> {
    let crs = resource_template(&[memory32_fixed(tpm.crb_base, u64::from(tpm.crb_len))?])?;
    let body = [
        name("_HID", &string(TPM_HID)?)?,
        name("_UID", &integer(0))?,
        name("_CRS", &crs)?,
    ]
    .concat();
    let aml = device("TPM2", &body)?;
    tracing::debug!(target: "ternvale::acpi", bytes = aml.len(), "dsdt tpm device built");
    Ok(aml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::sample;
    use crate::sdt::byte_sum;

    #[test]
    fn table_is_revision_4_crb_pointing_at_the_control_area() {
        let bytes = tpm2(&sample(1).tpm).expect("tpm2");
        assert_eq!(bytes.len(), TPM2_LEN);
        assert_eq!(&bytes[..4], b"TPM2");
        assert_eq!(bytes[8], 4);
        assert_eq!(byte_sum(&bytes), 0);
        assert_eq!(&bytes[36..40], &[0; 4], "client, reserved");
        assert_eq!(&bytes[40..48], &0x0914_0040u64.to_le_bytes());
        assert_eq!(&bytes[48..52], &7u32.to_le_bytes());
        assert_eq!(&bytes[52..76], &[0; 24], "params, LAML, LASA");
        assert_eq!(
            decode_tpm2(&bytes).expect("decode"),
            Tpm2Info {
                platform_class: 0,
                control_area: 0x0914_0040,
                start_method: 7
            }
        );
    }

    #[test]
    fn decode_rejects_other_tables_and_short_ones() {
        let config = sample(1);
        let mcfg = crate::mcfg::mcfg(&config.ecam).expect("mcfg");
        assert!(decode_tpm2(&mcfg).is_err());
        let mut short = tpm2(&config.tpm).expect("tpm2")[..48].to_vec();
        short[4..8].copy_from_slice(&48u32.to_le_bytes());
        assert!(decode_tpm2(&short).is_err());
    }

    #[test]
    fn device_has_the_msft0101_hid_and_the_crb_window() {
        let aml = tpm_device(&sample(1).tpm).expect("device");
        assert_eq!(&aml[..2], &[0x5b, 0x82], "DeviceOp");
        let hid = string("MSFT0101").expect("string");
        assert!(aml.windows(hid.len()).any(|w| w == hid.as_slice()));
        let window = memory32_fixed(0x0914_0000, 0x5000).expect("descriptor");
        assert!(aml.windows(window.len()).any(|w| w == window.as_slice()));
        assert!(aml.windows(4).any(|w| w == b"TPM2"));
    }
}
