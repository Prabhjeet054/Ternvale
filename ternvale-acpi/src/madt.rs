//! Multiple APIC Description Table for a GICv3 guest.
//!
//! ACPI 6.5 §5.2.12: signature `APIC`. After the SDT header come the Local
//! Interrupt Controller Address (0 on Arm) and Flags (0: no PC-AT 8259s), then
//! interrupt controller structures:
//!
//! - One GICC (type 0x0B, §5.2.12.14) per vCPU, 82 bytes in ACPI 6.5 (the
//!   TRBE interrupt field made it 82). ACPI Processor UID = vCPU index,
//!   MPIDR = the affinity fields of the vCPU's `MPIDR_EL1` (bits 31:24 and
//!   63:40 zero, so not its RES1 bit 31), flags = Enabled. The
//!   CPU Interface Number, GICC/GICV/GICH bases, and GICR base are 0: GICv3
//!   uses system registers, and the GICR structure describes the
//!   redistributors. Performance (PMU) and VGIC maintenance interrupts are 0
//!   because the DTB has no PMU node and no maintenance interrupt.
//! - One GICD (type 0x0C, §5.2.12.15), 24 bytes, GIC version 3.
//! - One GICR (type 0x0E, §5.2.12.17), 16 bytes: the redistributor discovery
//!   range, the same window as the DTB `intc` node's second `reg` entry.
//!
//! TODO(verify): MADT revision 6 for ACPI 6.5 (Table 5.19) and the 82-byte GICC
//! against what Windows on Arm accepts; QEMU `virt` emits revision 4 with
//! 80-byte GICCs, which Linux also accepts.

use crate::config::AcpiConfig;
use crate::sdt::{table, SdtHeader};
use crate::AcpiError;

/// MADT signature.
pub const MADT_SIGNATURE: [u8; 4] = *b"APIC";
/// MADT revision for ACPI 6.5.
pub const MADT_REVISION: u8 = 6;
/// GICC structure type.
pub const GICC_TYPE: u8 = 0x0b;
/// GICC length in ACPI 6.5.
pub const GICC_LEN: usize = 82;
/// GICD structure type.
pub const GICD_TYPE: u8 = 0x0c;
/// GICD length.
pub const GICD_LEN: usize = 24;
/// GICR structure type.
pub const GICR_TYPE: u8 = 0x0e;
/// GICR length.
pub const GICR_LEN: usize = 16;
/// GICC flags bit 0: the processor is usable.
pub const GICC_ENABLED: u32 = 1;
/// GICD `GIC version` value for GICv3.
pub const GIC_VERSION_3: u8 = 3;
/// Bytes between the SDT header and the first structure.
pub const MADT_FIXED_LEN: usize = 8;

/// Offsets inside a GICC structure.
pub mod gicc {
    /// ACPI Processor UID (u32).
    pub const UID: usize = 8;
    /// Flags (u32).
    pub const FLAGS: usize = 12;
    /// GICR base address (u64).
    pub const GICR_BASE: usize = 60;
    /// MPIDR (u64).
    pub const MPIDR: usize = 68;
}

/// The MADT for `config`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(cpus = config.mpidrs.len()))]
pub fn madt(config: &AcpiConfig) -> Result<Vec<u8>, AcpiError> {
    config.validate()?;
    let mut body = vec![0u8; MADT_FIXED_LEN];
    for (index, mpidr) in config.mpidrs.iter().enumerate() {
        let uid = u32::try_from(index).map_err(|_| AcpiError::BadConfig {
            reason: format!("cpu index {index} does not fit a UID"),
        })?;
        let mut gicc = [0u8; GICC_LEN];
        gicc[0] = GICC_TYPE;
        gicc[1] = GICC_LEN as u8;
        gicc[gicc::UID..gicc::UID + 4].copy_from_slice(&uid.to_le_bytes());
        gicc[gicc::FLAGS..gicc::FLAGS + 4].copy_from_slice(&GICC_ENABLED.to_le_bytes());
        gicc[gicc::MPIDR..gicc::MPIDR + 8].copy_from_slice(&mpidr.to_le_bytes());
        body.extend_from_slice(&gicc);
        tracing::debug!(target: "ternvale::acpi", uid, mpidr = %format!("{mpidr:#x}"), "madt gicc");
    }
    let mut gicd = [0u8; GICD_LEN];
    gicd[0] = GICD_TYPE;
    gicd[1] = GICD_LEN as u8;
    gicd[8..16].copy_from_slice(&config.gic.dist_base.to_le_bytes());
    gicd[20] = GIC_VERSION_3;
    body.extend_from_slice(&gicd);
    let mut gicr = [0u8; GICR_LEN];
    gicr[0] = GICR_TYPE;
    gicr[1] = GICR_LEN as u8;
    gicr[4..12].copy_from_slice(&config.gic.redist_base.to_le_bytes());
    gicr[12..16].copy_from_slice(&config.gic.redist_len.to_le_bytes());
    body.extend_from_slice(&gicr);
    tracing::debug!(
        target: "ternvale::acpi",
        gicd = %format!("{:#x}", config.gic.dist_base),
        gicr = %format!("{:#x}", config.gic.redist_base),
        gicr_len = %format!("{:#x}", config.gic.redist_len),
        "madt gic"
    );
    table(SdtHeader::new(MADT_SIGNATURE, MADT_REVISION), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::sample;
    use crate::sdt::{byte_sum, SDT_HEADER_LEN};

    #[test]
    fn madt_has_one_gicc_per_cpu_then_gicd_and_gicr() {
        let bytes = madt(&sample(3)).expect("madt");
        assert_eq!(
            bytes.len(),
            SDT_HEADER_LEN + 8 + 3 * GICC_LEN + GICD_LEN + GICR_LEN
        );
        assert_eq!(byte_sum(&bytes), 0);
        assert_eq!(
            &bytes[36..44],
            &[0; 8],
            "local controller address and flags"
        );
        let mut at = SDT_HEADER_LEN + MADT_FIXED_LEN;
        for cpu in 0..3u32 {
            let s = &bytes[at..at + GICC_LEN];
            assert_eq!((s[0], s[1]), (GICC_TYPE, 82));
            assert_eq!(u32::from_le_bytes(s[8..12].try_into().unwrap()), cpu);
            assert_eq!(u32::from_le_bytes(s[12..16].try_into().unwrap()), 1);
            assert_eq!(
                u64::from_le_bytes(s[68..76].try_into().unwrap()),
                u64::from(cpu)
            );
            assert_eq!(&s[60..68], &[0; 8], "gicr base");
            at += GICC_LEN;
        }
        let gicd = &bytes[at..at + GICD_LEN];
        assert_eq!((gicd[0], gicd[1], gicd[20]), (GICD_TYPE, 24, 3));
        assert_eq!(
            u64::from_le_bytes(gicd[8..16].try_into().unwrap()),
            0x0800_0000
        );
        let gicr = &bytes[at + GICD_LEN..];
        assert_eq!((gicr[0], gicr[1]), (GICR_TYPE, 16));
        assert_eq!(
            u64::from_le_bytes(gicr[4..12].try_into().unwrap()),
            0x080a_0000
        );
        assert_eq!(
            u32::from_le_bytes(gicr[12..16].try_into().unwrap()),
            0x00f6_0000
        );
    }

    #[test]
    fn gicc_count_and_length_follow_the_cpu_count() {
        for cpus in [1u32, 2, 16, 123, crate::config::MAX_CPUS as u32] {
            let bytes = madt(&sample(cpus)).expect("madt");
            let n = cpus as usize;
            assert_eq!(bytes.len(), 36 + 8 + n * GICC_LEN + GICD_LEN + GICR_LEN);
            let mut at = SDT_HEADER_LEN + MADT_FIXED_LEN;
            let mut giccs = 0;
            while at < bytes.len() {
                giccs += usize::from(bytes[at] == GICC_TYPE);
                at += usize::from(bytes[at + 1]);
            }
            assert_eq!(giccs, n, "{cpus} cpus");
        }
    }

    #[test]
    fn invalid_config_is_rejected() {
        assert!(madt(&sample(0)).is_err());
    }
}
