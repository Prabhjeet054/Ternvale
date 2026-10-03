use super::*;
use ternvale_acpi::AcpiTables;
use ternvale_vmm::acpi_config;

const RAM_BASE: u64 = 0x4000_0000;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ternvale-cli-acpi-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// A fake QEMU RAM image: Ternvale-built tables at an offset inside RAM.
fn ram_image(dir: &Path) -> (std::path::PathBuf, AcpiTables) {
    let tables = AcpiTables::build(RAM_BASE + 0x2000, 0x1000, &acpi_config(1)).expect("build");
    let mut image = vec![0u8; 0x4000];
    for table in tables.tables() {
        let at = (table.gpa - RAM_BASE) as usize;
        image[at..at + table.bytes.len()].copy_from_slice(&table.bytes);
    }
    let path = dir.join("ram.bin");
    std::fs::write(&path, image).expect("write image");
    (path, tables)
}

#[test]
fn qemu_image_is_walked_and_written_per_signature() {
    let dir = scratch("walk");
    let (ram, tables) = ram_image(&dir);
    let found = qemu_tables(&ram, RAM_BASE).expect("walk image");
    assert_eq!(found.len(), tables.tables().len());
    write_set(&dir.join("qemu"), &found).expect("write set");
    for name in [
        "RSDP.dat",
        "XSDT.dat",
        "FACP.dat",
        "DSDT.dat",
        "APIC.dat",
        "GTDT.dat",
        "MCFG.dat",
        "SPCR.dat",
        "DBG2.dat",
        "tables.txt",
    ] {
        assert!(dir.join("qemu").join(name).exists(), "{name}");
    }
    let fadt = std::fs::read(dir.join("qemu/FACP.dat")).expect("read FACP");
    assert_eq!(fadt, tables.tables()[2].bytes);
    let listing = std::fs::read_to_string(dir.join("qemu/tables.txt")).expect("listing");
    assert!(listing.contains("FACP"), "{listing}");
    assert!(listing.contains("checksum ok"), "{listing}");
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn repeated_signatures_get_distinct_files() {
    let dir = scratch("dupes");
    let ssdt = |gpa| DumpedTable {
        signature: "SSDT".to_string(),
        gpa,
        bytes: vec![1, 2, 3],
    };
    write_set(&dir, &[ssdt(0x10), ssdt(0x20), ssdt(0x30)]).expect("write set");
    for name in ["SSDT.dat", "SSDT-2.dat", "SSDT-3.dat"] {
        assert!(dir.join(name).exists(), "{name}");
    }
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn image_without_tables_names_the_file() {
    let dir = scratch("empty");
    let path = dir.join("ram.bin");
    std::fs::write(&path, vec![0u8; 512]).expect("write");
    let error = qemu_tables(&path, RAM_BASE).unwrap_err();
    let text = crate::error_chain(&error);
    assert!(text.contains("ram.bin"), "{text}");
    assert!(text.contains("no valid RSDP"), "{text}");
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn acpi_dump_parses_hex_and_decimal_ram_bases() {
    use crate::cli::{Cli, Command};
    use clap::Parser;
    let parse = |args: &[&str]| Cli::try_parse_from(args).map(|cli| cli.command);
    assert_eq!(
        parse(&["ternvale", "acpi-dump", "--out", "d"]).expect("defaults"),
        Command::AcpiDump {
            out: "d".into(),
            qemu_ram: None,
            ram_base: 0x4000_0000,
            cpus: 1,
        }
    );
    assert!(matches!(
        parse(&["ternvale", "acpi-dump", "--out", "d", "--cpus", "4"]),
        Ok(Command::AcpiDump { cpus: 4, .. })
    ));
    let command = parse(&[
        "ternvale",
        "acpi-dump",
        "--out",
        "d",
        "--qemu-ram",
        "r.bin",
        "--ram-base",
        "4096",
    ])
    .expect("decimal");
    assert!(
        matches!(command, Command::AcpiDump { ram_base: 4096, .. }),
        "{command:?}"
    );
    assert!(parse(&["ternvale", "acpi-dump", "--out", "d", "--ram-base", "0xzz"]).is_err());
}

#[test]
fn comparison_lists_one_sided_signatures() {
    let built = AcpiTables::build(0x0910_0000, 0x2_0000, &acpi_config(1)).expect("build");
    let ours: Vec<DumpedTable> = built
        .tables()
        .iter()
        .map(|t| DumpedTable {
            signature: t.signature.to_string(),
            gpa: t.gpa,
            bytes: t.bytes.clone(),
        })
        .collect();
    let mut theirs = ours.clone();
    theirs.retain(|t| t.signature != "DSDT");
    let mut pptt = ours[1].clone();
    pptt.signature = "PPTT".to_string();
    theirs.push(pptt);
    let text = comparison(&ours, &theirs);
    assert!(
        text.contains("| Signature | Ternvale | QEMU -M virt |"),
        "{text}"
    );
    assert!(text.contains("Only in Ternvale: DSDT"), "{text}");
    assert!(text.contains("Only in QEMU: PPTT"), "{text}");
}
