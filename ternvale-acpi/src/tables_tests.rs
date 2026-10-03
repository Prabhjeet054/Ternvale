use std::io::Write;
use std::sync::{Arc, Mutex};

use super::*;
use crate::fadt::X_DSDT_OFFSET;
use crate::rsdp::{rsdp_checksums_ok, RSDP_XSDT_OFFSET};
use crate::sdt::{SdtHeader, SDT_HEADER_LEN};

const BASE: u64 = 0x0910_0000;
const SIZE: u64 = 0x2_0000;

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn built() -> AcpiTables {
    AcpiTables::build(BASE, SIZE, PsciConduit::Hvc).expect("build")
}

#[test]
fn tables_are_ordered_aligned_and_do_not_overlap() {
    let tables = built();
    let names: Vec<_> = tables.tables().iter().map(|t| t.signature).collect();
    assert_eq!(names, ["RSD PTR ", "XSDT", "FACP", "DSDT"]);
    assert_eq!(tables.rsdp_gpa(), BASE);
    let mut end = BASE;
    for table in tables.tables() {
        assert!(table.gpa >= end, "{} overlaps", table.signature);
        assert_eq!(table.gpa % TABLE_ALIGN, 0, "{}", table.signature);
        end = table.gpa + table.bytes.len() as u64;
    }
    assert_eq!(tables.used_bytes(), end - BASE);
    assert!(tables.used_bytes() <= SIZE);
}

#[test]
fn pointers_chain_rsdp_to_dsdt() {
    let tables = built();
    let [rsdp, xsdt, fadt, dsdt] = tables.tables() else {
        panic!("expected four tables");
    };
    assert_eq!(u64_at(&rsdp.bytes, RSDP_XSDT_OFFSET), xsdt.gpa);
    assert_eq!(xsdt.bytes.len(), SDT_HEADER_LEN + 8);
    assert_eq!(u64_at(&xsdt.bytes, SDT_HEADER_LEN), fadt.gpa);
    assert_eq!(u64_at(&fadt.bytes, X_DSDT_OFFSET), dsdt.gpa);
}

#[test]
fn every_table_checksum_holds() {
    let tables = built();
    for table in tables.tables() {
        assert_eq!(byte_sum(&table.bytes), 0, "{}", table.signature);
        if table.signature == "RSD PTR " {
            let bytes: [u8; RSDP_LEN] = table.bytes.as_slice().try_into().unwrap();
            assert!(rsdp_checksums_ok(&bytes));
            continue;
        }
        let header = SdtHeader::parse(&table.bytes).expect("header");
        assert_eq!(&header.signature, table.signature.as_bytes());
        assert_eq!(header.length as usize, table.bytes.len());
        assert_eq!(header.checksum, table.checksum());
    }
}

#[test]
fn bad_regions_are_rejected() {
    let error = AcpiTables::build(BASE, 64, PsciConduit::Hvc).unwrap_err();
    assert!(
        matches!(error, AcpiError::RegionTooSmall { have: 64, .. }),
        "{error}"
    );
    let error = AcpiTables::build(BASE + 8, SIZE, PsciConduit::Hvc).unwrap_err();
    assert_eq!(error, AcpiError::Misaligned { base: BASE + 8 });
    let top = u64::MAX - 0xf;
    let error = AcpiTables::build(top, SIZE, PsciConduit::Hvc).unwrap_err();
    assert_eq!(error, AcpiError::AddressOverflow { base: top });
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("captured").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn write_copies_every_table_and_logs_it_at_info() {
    // A second live dispatcher keeps tracing from caching these callsites as
    // disabled when a parallel test without a subscriber hits them first.
    let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let tables = built();
    let mut guest = vec![0u8; SIZE as usize];
    tracing::subscriber::with_default(subscriber, || {
        tables
            .write(|gpa, bytes| -> Result<(), String> {
                let at = (gpa - BASE) as usize;
                guest[at..at + bytes.len()].copy_from_slice(bytes);
                Ok(())
            })
            .expect("write");
    });
    for table in tables.tables() {
        let at = (table.gpa - BASE) as usize;
        assert_eq!(&guest[at..at + table.bytes.len()], table.bytes.as_slice());
    }
    let text = String::from_utf8(captured.0.lock().expect("captured").clone()).expect("utf8");
    for table in tables.tables() {
        let line = text
            .lines()
            .find(|line| line.contains(&format!("signature=\"{}\"", table.signature)))
            .unwrap_or_else(|| panic!("{} not logged in {text}", table.signature));
        assert!(line.contains(" INFO "), "{line}");
        assert!(line.contains("acpi table written"), "{line}");
        assert!(
            line.contains(&format!("length={}", table.bytes.len())),
            "{line}"
        );
        assert!(
            line.contains(&format!("checksum={:#04x}", table.checksum())),
            "{line}"
        );
        assert!(line.contains(&format!("gpa={:#x}", table.gpa)), "{line}");
        assert!(line.contains("sum_ok=true"), "{line}");
    }
    assert!(text.contains("acpi tables written"), "{text}");
}

#[test]
fn writer_failure_names_the_table() {
    let tables = built();
    let mut calls = 0;
    let error = tables
        .write(|_, _| {
            calls += 1;
            if calls == 2 {
                Err("guest memory gone")
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(
        error,
        AcpiError::Write {
            signature: "XSDT".to_string(),
            gpa: tables.tables()[1].gpa,
            reason: "guest memory gone".to_string(),
        }
    );
}
