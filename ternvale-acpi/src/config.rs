//! What the tables describe: CPUs, GIC, timer, ECAM, and UART.
//!
//! The VMM fills this from its platform map (`ternvale-vmm` `platform.rs`,
//! `smp::mpidr`, the DTB's interrupt numbers), so this crate holds no
//! addresses of its own. Interrupt numbers are the DTB's: a PPI or SPI index,
//! converted to a GSIV (the GIC INTID) with [`ppi_gsiv`] and [`spi_gsiv`]
//! (ACPI 6.5 §5.2.12.14 notes; GIC architecture: PPIs are INTIDs 16–31, SPIs
//! start at 32).

use crate::fadt::PsciConduit;
use crate::AcpiError;

/// Largest GICC count the MADT builder accepts.
pub const MAX_CPUS: usize = 512;
/// Bits a GICC MPIDR may carry: Aff3 (39:32), Aff2, Aff1, Aff0 (23:0). ACPI
/// 6.5 §5.2.12.14 requires bits 63:40 and 31:24 to be zero, so `MPIDR_EL1`'s
/// RES1 bit 31 and MT/U bits must be masked off by the caller.
pub const MPIDR_AFFINITY: u64 = 0xff_00ff_ffff;
/// First SPI INTID.
const SPI_BASE: u32 = 32;
/// First PPI INTID.
const PPI_BASE: u32 = 16;
/// Last SPI INTID (GICv3: 1019).
const SPI_LAST: u32 = 1019;

/// Everything the tables point at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpiConfig {
    /// PSCI conduit (FADT ARM boot flags).
    pub conduit: PsciConduit,
    /// Affinity fields ([`MPIDR_AFFINITY`]) of each vCPU's `MPIDR_EL1`, in
    /// ACPI Processor UID order (UID = index).
    pub mpidrs: Vec<u64>,
    /// GICv3 distributor and redistributor windows.
    pub gic: GicConfig,
    /// Architected timer interrupts.
    pub timer: TimerConfig,
    /// PCIe ECAM window.
    pub ecam: EcamConfig,
    /// PL011 console and debug UART.
    pub uart: UartConfig,
}

/// GICv3 windows (MADT GICD and GICR structures).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GicConfig {
    /// Distributor base.
    pub dist_base: u64,
    /// Redistributor discovery range base.
    pub redist_base: u64,
    /// Redistributor discovery range length.
    pub redist_len: u32,
}

/// Timer PPIs in DTB order: secure physical, non-secure physical, virtual,
/// hypervisor (`arm,armv8-timer` binding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerConfig {
    /// DTB PPI index of the four timers.
    pub ppis: [u32; 4],
    /// The DTB's `always-on` property.
    pub always_on: bool,
}

/// One ECAM window on PCI segment 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcamConfig {
    /// ECAM base for `start_bus`.
    pub base: u64,
    /// First bus decoded.
    pub start_bus: u8,
    /// Last bus decoded.
    pub end_bus: u8,
}

/// The PL011.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartConfig {
    /// Register block base.
    pub base: u64,
    /// Register block length (DBG2 address size).
    pub len: u32,
    /// DTB SPI index of its interrupt.
    pub spi: u32,
}

/// GSIV of DTB PPI `ppi`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(ppi))]
pub fn ppi_gsiv(ppi: u32) -> u32 {
    PPI_BASE + ppi
}

/// GSIV of DTB SPI `spi`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(spi))]
pub fn spi_gsiv(spi: u32) -> u32 {
    SPI_BASE + spi
}

impl AcpiConfig {
    /// Reject values no table can encode or no guest could use.
    #[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(cpus = self.mpidrs.len()))]
    pub fn validate(&self) -> Result<(), AcpiError> {
        let fail = |reason: String| {
            tracing::warn!(target: "ternvale::acpi", reason = %reason, "acpi config rejected");
            Err(AcpiError::BadConfig { reason })
        };
        if self.mpidrs.is_empty() || self.mpidrs.len() > MAX_CPUS {
            return fail(format!("{} cpus (1..={MAX_CPUS})", self.mpidrs.len()));
        }
        for (index, mpidr) in self.mpidrs.iter().enumerate() {
            if mpidr & !MPIDR_AFFINITY != 0 {
                return fail(format!(
                    "cpu {index} MPIDR {mpidr:#x} has bits outside the affinity fields"
                ));
            }
            if self.mpidrs[..index].contains(mpidr) {
                return fail(format!("cpu {index} repeats MPIDR {mpidr:#x}"));
            }
        }
        if let Some(ppi) = self.timer.ppis.iter().find(|ppi| **ppi > 15) {
            return fail(format!("timer PPI {ppi} is not 0..=15"));
        }
        if spi_gsiv(self.uart.spi) > SPI_LAST {
            return fail(format!(
                "uart SPI {} is past INTID {SPI_LAST}",
                self.uart.spi
            ));
        }
        if self.ecam.end_bus < self.ecam.start_bus {
            return fail(format!(
                "ecam buses {}..={}",
                self.ecam.start_bus, self.ecam.end_bus
            ));
        }
        if self.gic.redist_len == 0 || self.uart.len == 0 {
            return fail("zero-length redistributor or uart window".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A config with test-only values (not Ternvale's platform map).
    pub(crate) fn sample(cpus: u32) -> AcpiConfig {
        AcpiConfig {
            conduit: PsciConduit::Hvc,
            mpidrs: (0..u64::from(cpus))
                .map(|i| ((i / 16) << 8) | (i % 16))
                .collect(),
            gic: GicConfig {
                dist_base: 0x0800_0000,
                redist_base: 0x080a_0000,
                redist_len: 0x00f6_0000,
            },
            timer: TimerConfig {
                ppis: [13, 14, 11, 10],
                always_on: true,
            },
            ecam: EcamConfig {
                base: 0x3f00_0000,
                start_bus: 0,
                end_bus: 15,
            },
            uart: UartConfig {
                base: 0x0900_0000,
                len: 0x1000,
                spi: 1,
            },
        }
    }

    #[test]
    fn gsivs_follow_the_gic_intid_map() {
        assert_eq!(ppi_gsiv(13), 29);
        assert_eq!(ppi_gsiv(10), 26);
        assert_eq!(spi_gsiv(1), 33);
    }

    #[test]
    fn sample_is_valid_and_bad_values_are_named() {
        sample(4).validate().expect("valid");
        let reject = |config: AcpiConfig, needle: &str| {
            let error = config.validate().unwrap_err().to_string();
            assert!(error.contains(needle), "{error}");
        };
        reject(sample(0), "0 cpus");
        let mut dup = sample(2);
        dup.mpidrs[1] = dup.mpidrs[0];
        reject(dup, "repeats MPIDR");
        let mut res1 = sample(1);
        res1.mpidrs[0] = 1 << 31;
        reject(res1, "outside the affinity fields");
        let mut ppi = sample(1);
        ppi.timer.ppis[2] = 16;
        reject(ppi, "timer PPI 16");
        let mut spi = sample(1);
        spi.uart.spi = 988;
        reject(spi, "uart SPI 988");
        let mut buses = sample(1);
        buses.ecam.end_bus = 0;
        buses.ecam.start_bus = 1;
        reject(buses, "ecam buses 1..=0");
        let mut zero = sample(1);
        zero.gic.redist_len = 0;
        reject(zero, "zero-length");
    }
}
