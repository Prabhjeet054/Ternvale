//! Check a table set read back from guest memory against the config it was
//! built from: every address, interrupt, and CPU the tables carry must be the
//! config's. A mismatch is one line naming the table, field, expected and
//! found values; any mismatch fails with [`AcpiError::Drift`].

use crate::config::{ppi_gsiv, spi_gsiv, AcpiConfig};
use crate::dbg2::{PORT_SERIAL, SUBTYPE_PL011};
use crate::decode::{decode_dbg2, decode_gtdt, decode_madt, decode_mcfg, decode_spcr};
use crate::dump::DumpedTable;
use crate::gas::Gas;
use crate::madt::{GICC_ENABLED, GIC_VERSION_3};
use crate::spcr::{INTERFACE_PL011, INTERRUPT_TYPE_GIC};
use crate::tables::SIGNATURES;
use crate::AcpiError;

/// Collects mismatches.
struct Problems(Vec<String>);

impl Problems {
    fn expect<T: PartialEq + std::fmt::Debug>(&mut self, what: &str, expected: T, found: T) {
        if expected != found {
            self.0
                .push(format!("{what}: expected {expected:x?}, found {found:x?}"));
        }
    }

    fn decoded<T>(&mut self, what: &str, result: Result<T, AcpiError>) -> Option<T> {
        result
            .map_err(|error| self.0.push(format!("{what}: {error}")))
            .ok()
    }
}

/// Fail unless `tables` are exactly the tables `expected` describes.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(tables = tables.len(), cpus = expected.mpidrs.len())
)]
pub fn verify(tables: &[DumpedTable], expected: &AcpiConfig) -> Result<(), AcpiError> {
    let mut p = Problems(Vec::new());
    let found: Vec<&str> = tables.iter().map(|t| t.signature.as_str()).collect();
    let mut want = SIGNATURES.to_vec();
    let mut have = found.clone();
    want.sort_unstable();
    have.sort_unstable();
    p.expect("table set", want, have);
    for table in tables.iter().filter(|t| !t.checksum_ok()) {
        p.0.push(format!(
            "{}: checksum does not sum to zero",
            table.signature
        ));
    }
    let bytes = |signature: &str| {
        tables
            .iter()
            .find(|t| t.signature == signature)
            .map(|t| t.bytes.as_slice())
    };

    if let Some(madt) = bytes("APIC").and_then(|b| p.decoded("APIC", decode_madt(b))) {
        p.expect(
            "APIC GICD (base, version)",
            vec![(expected.gic.dist_base, GIC_VERSION_3)],
            madt.gicds,
        );
        p.expect(
            "APIC GICR (base, length)",
            vec![(expected.gic.redist_base, expected.gic.redist_len)],
            madt.gicrs,
        );
        p.expect("APIC GICC count", expected.mpidrs.len(), madt.giccs.len());
        for (index, (gicc, mpidr)) in madt.giccs.iter().zip(&expected.mpidrs).enumerate() {
            p.expect(&format!("APIC GICC {index} UID"), index as u32, gicc.uid);
            p.expect(&format!("APIC GICC {index} MPIDR"), *mpidr, gicc.mpidr);
            p.expect(
                &format!("APIC GICC {index} flags"),
                GICC_ENABLED,
                gicc.flags,
            );
            p.expect(&format!("APIC GICC {index} GICR base"), 0, gicc.gicr_base);
        }
    }
    if let Some(gtdt) = bytes("GTDT").and_then(|b| p.decoded("GTDT", decode_gtdt(b))) {
        p.expect(
            "GTDT timer GSIVs",
            expected.timer.ppis.map(ppi_gsiv),
            gtdt.gsivs,
        );
        let always_on = gtdt
            .flags
            .map(|flags| flags & crate::gtdt::TIMER_ALWAYS_ON != 0);
        p.expect("GTDT always-on", [expected.timer.always_on; 4], always_on);
        p.expect(
            "GTDT level/active-high",
            [0; 4],
            gtdt.flags.map(|flags| flags & 0b11),
        );
    }
    if let Some(mcfg) = bytes("MCFG").and_then(|b| p.decoded("MCFG", decode_mcfg(b))) {
        let found: Vec<_> = mcfg
            .iter()
            .map(|a| {
                (
                    a.base + (u64::from(a.start_bus) << 20),
                    a.segment,
                    a.start_bus,
                    a.end_bus,
                )
            })
            .collect();
        let ecam = expected.ecam;
        p.expect(
            "MCFG allocations (base, segment, start bus, end bus)",
            vec![(ecam.base, 0, ecam.start_bus, ecam.end_bus)],
            found,
        );
    }
    let uart = Gas::mmio32(expected.uart.base);
    if let Some(spcr) = bytes("SPCR").and_then(|b| p.decoded("SPCR", decode_spcr(b))) {
        p.expect("SPCR interface type", INTERFACE_PL011, spcr.interface_type);
        p.expect("SPCR base", uart, spcr.base);
        p.expect(
            "SPCR interrupt type",
            INTERRUPT_TYPE_GIC,
            spcr.interrupt_type,
        );
        p.expect("SPCR GSIV", spi_gsiv(expected.uart.spi), spcr.gsiv);
    }
    if let Some(dbg2) = bytes("DBG2").and_then(|b| p.decoded("DBG2", decode_dbg2(b))) {
        let found: Vec<_> = dbg2
            .iter()
            .map(|d| (d.port_type, d.subtype, d.base, d.size))
            .collect();
        p.expect(
            "DBG2 devices (type, subtype, base, size)",
            vec![(PORT_SERIAL, SUBTYPE_PL011, uart, expected.uart.len)],
            found,
        );
    }

    if p.0.is_empty() {
        tracing::debug!(target: "ternvale::acpi", tables = tables.len(), "acpi tables match the config");
        return Ok(());
    }
    for problem in &p.0 {
        tracing::error!(target: "ternvale::acpi", problem = %problem, "acpi table drift");
    }
    Err(AcpiError::Drift { problems: p.0 })
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
