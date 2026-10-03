use std::collections::BTreeMap;

use super::*;
use crate::config::tests::sample;
use crate::loader_command::FNAME_LEN;
use crate::sdt::byte_sum;
use crate::tables::AcpiTables;

/// What EDK2's `InstallQemuFwCfgTables` ends up with: each file at its
/// allocated address, and the tables handed to `InstallAcpiTable`.
struct Installed {
    blobs: BTreeMap<String, (u64, Vec<u8>)>,
    tables: Vec<(String, Vec<u8>)>,
}

/// `QemuFwCfgAcpi.c` (edk2-stable202308), pass 1 then pass 2, with its bounds
/// checks. `bases` maps file name to the address `AllocatePages` returned.
fn edk2_install(files: &[(String, Vec<u8>)], bases: &[(&str, u64)]) -> Result<Installed, String> {
    let file = |name: &str| {
        files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.clone())
    };
    let loader = file(LOADER_FILE).ok_or("PlatformHasAcpiDtDxe: no etc/table-loader, DT chosen")?;
    if loader.len() % LOADER_ENTRY_LEN != 0 {
        return Err("etc/table-loader has invalid size".into());
    }
    let commands: Vec<LoaderCommand> = loader
        .chunks(LOADER_ENTRY_LEN)
        .map(|entry| LoaderCommand::decode(entry).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let mut blobs: BTreeMap<String, (u64, Vec<u8>)> = BTreeMap::new();
    for command in &commands {
        match command {
            LoaderCommand::Allocate {
                file: name, align, ..
            } => {
                if *align > 4096 {
                    return Err(format!("unsupported alignment {align:#x}"));
                }
                let bytes = file(name).ok_or(format!("QemuFwCfgFindFile({name}) failed"))?;
                let base = bases
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, b)| *b)
                    .ok_or("no base")?;
                assert_eq!(base % u64::from(*align), 0);
                if blobs.insert(name.clone(), (base, bytes)).is_some() {
                    return Err(format!("duplicated file {name}"));
                }
            }
            LoaderCommand::AddPointer {
                pointer,
                pointee,
                offset,
                size,
            } => {
                let pointee_base = blobs.get(pointee).ok_or("invalid pointee")?.0;
                let pointee_len = blobs[pointee].1.len() as u64;
                let blob = &mut blobs.get_mut(pointer).ok_or("invalid pointer file")?.1;
                let (at, size) = (*offset as usize, usize::from(*size));
                if ![1, 2, 4, 8].contains(&size) || blob.len() < size || blob.len() - size < at {
                    return Err("invalid pointer location or size".into());
                }
                let mut value = [0u8; 8];
                value[..size].copy_from_slice(&blob[at..at + size]);
                let value = u64::from_le_bytes(value);
                if value >= pointee_len {
                    return Err(format!("invalid pointer value {value:#x} in {pointer}"));
                }
                let value = value + pointee_base;
                if size < 8 && value >> (size * 8) != 0 {
                    return Err("relocated pointer value unrepresentable".into());
                }
                blob[at..at + size].copy_from_slice(&value.to_le_bytes()[..size]);
            }
            LoaderCommand::AddChecksum {
                file: name,
                result,
                start,
                len,
            } => {
                let blob = &mut blobs.get_mut(name).ok_or("invalid blob")?.1;
                let (result, start, len) = (*result as usize, *start as usize, *len as usize);
                if blob.len() <= result || blob.len() < len || blob.len() - len < start {
                    return Err("invalid checksum range".into());
                }
                blob[result] = 0u8.wrapping_sub(byte_sum(&blob[start..start + len]));
            }
        }
    }
    let mut tables = Vec::new();
    let mut seen = Vec::new();
    for command in &commands {
        let LoaderCommand::AddPointer {
            pointer,
            pointee,
            offset,
            size,
        } = command
        else {
            continue;
        };
        let (_, blob) = &blobs[pointer];
        let mut value = [0u8; 8];
        let at = *offset as usize;
        value[..usize::from(*size)].copy_from_slice(&blob[at..at + usize::from(*size)]);
        let value = u64::from_le_bytes(value);
        if seen.contains(&value) {
            continue;
        }
        seen.push(value);
        let (base, target) = &blobs[pointee];
        let rest = &target[(value - base) as usize..];
        if rest.len() < SDT_HEADER_LEN {
            continue;
        }
        let len = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        if len < SDT_HEADER_LEN || len > rest.len() || byte_sum(&rest[..len]) != 0 {
            continue;
        }
        let signature = String::from_utf8_lossy(&rest[..4]).to_string();
        if signature != "RSDT" && signature != "XSDT" {
            tables.push((signature, rest[..len].to_vec()));
        }
    }
    Ok(Installed { blobs, tables })
}

fn built(base: u64, cpus: u32) -> AcpiTables {
    AcpiTables::build(base, 0x2_0000, &sample(cpus)).expect("build")
}

fn blobs_for(tables: &AcpiTables) -> LoaderBlobs {
    let refs: Vec<TableRef<'_>> = tables.tables().iter().map(TableRef::from).collect();
    LoaderBlobs::build(&refs).expect("loader blobs")
}

#[test]
fn edk2_installs_every_table_except_the_xsdt_at_any_address() {
    for cpus in [1, 4, 16] {
        let source = built(0x0910_0000, cpus);
        let files = blobs_for(&source).files().expect("files");
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, [RSDP_FILE, TABLES_FILE, LOADER_FILE]);
        for tables_base in [0x4c4c_0000u64, 0x1_2345_6000] {
            let installed = edk2_install(
                &files,
                &[(RSDP_FILE, 0x4c4b_0000), (TABLES_FILE, tables_base)],
            )
            .expect("edk2 accepts the script");
            let signatures: Vec<&str> = installed.tables.iter().map(|(s, _)| s.as_str()).collect();
            assert_eq!(
                signatures,
                ["FACP", "APIC", "GTDT", "MCFG", "SPCR", "DBG2", "DSDT"]
            );
            let xsdt_gpa = source.tables()[1].gpa;
            let moved = |gpa: u64| gpa - xsdt_gpa + tables_base;
            let original = |signature: &str| {
                source
                    .tables()
                    .iter()
                    .find(|t| t.signature == signature)
                    .unwrap()
            };
            for (signature, bytes) in &installed.tables {
                let want = &original(signature).bytes;
                assert_eq!(byte_sum(bytes), 0, "{signature}");
                if signature == "FACP" {
                    let x_dsdt = u64::from_le_bytes(
                        bytes[X_DSDT_OFFSET..X_DSDT_OFFSET + 8].try_into().unwrap(),
                    );
                    assert_eq!(x_dsdt, moved(original("DSDT").gpa));
                    assert_eq!(bytes[..SDT_CHECKSUM_OFFSET], want[..SDT_CHECKSUM_OFFSET]);
                    assert_eq!(
                        bytes[SDT_CHECKSUM_OFFSET + 1..X_DSDT_OFFSET],
                        want[SDT_CHECKSUM_OFFSET + 1..X_DSDT_OFFSET]
                    );
                    assert_eq!(bytes[X_DSDT_OFFSET + 8..], want[X_DSDT_OFFSET + 8..]);
                } else {
                    assert_eq!(bytes, want, "{signature} at {tables_base:#x}, {cpus} cpus");
                }
            }
            let (_, blob) = &installed.blobs[TABLES_FILE];
            let xsdt = &blob[..source.tables()[1].bytes.len()];
            assert_eq!(byte_sum(xsdt), 0);
            let entries: Vec<u64> = xsdt[SDT_HEADER_LEN..]
                .chunks(8)
                .map(|e| u64::from_le_bytes(e.try_into().unwrap()))
                .collect();
            let listed: Vec<u64> = ["FACP", "APIC", "GTDT", "MCFG", "SPCR", "DBG2"]
                .iter()
                .map(|s| moved(original(s).gpa))
                .collect();
            assert_eq!(entries, listed);
            let (_, rsdp) = &installed.blobs[RSDP_FILE];
            assert!(crate::rsdp::rsdp_checksums_ok(
                rsdp.as_slice().try_into().unwrap()
            ));
            let xsdt_at = u64::from_le_bytes(
                rsdp[RSDP_XSDT_OFFSET..RSDP_XSDT_OFFSET + 8]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(xsdt_at, tables_base);
        }
    }
}

#[test]
fn blobs_hold_offsets_and_zero_checksums() {
    let source = built(0x0910_0000, 2);
    let blobs = blobs_for(&source);
    let xsdt_gpa = source.tables()[1].gpa;
    assert_eq!(&blobs.tables[..4], b"XSDT");
    assert_eq!(blobs.tables[SDT_CHECKSUM_OFFSET], 0);
    let rsdp_xsdt = u64::from_le_bytes(
        blobs.rsdp[RSDP_XSDT_OFFSET..RSDP_XSDT_OFFSET + 8]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        rsdp_xsdt, 0,
        "the XSDT is the first byte of etc/acpi/tables"
    );
    assert_eq!(
        (
            blobs.rsdp[RSDP_CHECKSUM_OFFSET],
            blobs.rsdp[RSDP_EXT_CHECKSUM_OFFSET]
        ),
        (0, 0)
    );
    for table in &source.tables()[1..] {
        let at = (table.gpa - xsdt_gpa) as usize;
        assert_eq!(&blobs.tables[at..at + 4], table.signature.as_bytes());
        assert_eq!(
            blobs.tables[at + SDT_CHECKSUM_OFFSET],
            0,
            "{}",
            table.signature
        );
    }
    let pointers = blobs
        .commands
        .iter()
        .filter(|c| matches!(c, LoaderCommand::AddPointer { .. }))
        .count();
    let checksums = blobs
        .commands
        .iter()
        .filter(|c| matches!(c, LoaderCommand::AddChecksum { .. }))
        .count();
    assert_eq!(
        pointers,
        6 + 1 + 1,
        "six XSDT entries, X_DSDT, RSDP XsdtAddress"
    );
    assert_eq!(checksums, 8 + 2, "eight SDTs, two RSDP checksums");
    assert!(
        matches!(&blobs.commands[0], LoaderCommand::Allocate { file, align: 16, zone: Zone::FSeg } if file == RSDP_FILE)
    );
    assert!(
        matches!(&blobs.commands[1], LoaderCommand::Allocate { file, align: 64, zone: Zone::High } if file == TABLES_FILE)
    );
}

#[test]
fn commands_round_trip_through_the_128_byte_layout() {
    let blobs = blobs_for(&built(0x0910_0000, 1));
    let loader = blobs.loader().expect("loader");
    assert_eq!(loader.len(), blobs.commands.len() * LOADER_ENTRY_LEN);
    for (entry, command) in loader.chunks(LOADER_ENTRY_LEN).zip(&blobs.commands) {
        assert_eq!(&LoaderCommand::decode(entry).expect("decode"), command);
    }
    let pointer = LoaderCommand::AddPointer {
        pointer: "a".into(),
        pointee: "b".into(),
        offset: 0x1234_5678,
        size: 8,
    }
    .encode()
    .expect("encode");
    assert_eq!(&pointer[..4], &2u32.to_le_bytes());
    assert_eq!((pointer[4], pointer[60]), (b'a', b'b'));
    assert_eq!(&pointer[116..121], &[0x78, 0x56, 0x34, 0x12, 8]);
}

#[test]
fn bad_inputs_are_rejected_with_a_reason() {
    let long = "x".repeat(FNAME_LEN);
    let error = LoaderCommand::Allocate {
        file: long,
        align: 1,
        zone: Zone::High,
    }
    .encode()
    .unwrap_err();
    assert!(error.to_string().contains("does not fit"), "{error}");
    let mut entry = [0u8; LOADER_ENTRY_LEN];
    entry[0] = 9;
    assert!(LoaderCommand::decode(&entry)
        .unwrap_err()
        .to_string()
        .contains("command type 9"));
    assert!(LoaderCommand::decode(&entry[..64]).is_err());

    let source = built(0x0910_0000, 1);
    let tables: Vec<TableRef<'_>> = source.tables().iter().map(TableRef::from).collect();
    let error = LoaderBlobs::build(&tables[1..]).unwrap_err();
    assert!(error.to_string().contains("exactly one RSDP"), "{error}");
    let mut fadt = source.tables()[2].bytes.clone();
    fadt[X_DSDT_OFFSET] ^= 0x08;
    let mut broken = tables.clone();
    broken[2] = TableRef {
        bytes: &fadt,
        ..tables[2]
    };
    let error = LoaderBlobs::build(&broken).unwrap_err();
    assert!(error.to_string().contains("FACP+0x8c"), "{error}");
    let mut overlap = tables.clone();
    overlap[3].gpa = overlap[2].gpa + 8;
    assert!(LoaderBlobs::build(&overlap)
        .unwrap_err()
        .to_string()
        .contains("overlaps"));
}

#[test]
fn a_missing_checksum_command_leaves_the_table_uninstalled() {
    let blobs = blobs_for(&built(0x0910_0000, 1));
    let mut files = blobs.files().expect("files");
    let mut commands = blobs.commands.clone();
    let apic_at = commands
        .iter()
        .position(|c| matches!(c, LoaderCommand::AddChecksum { file, start, .. } if file == TABLES_FILE && *start > 0x200))
        .expect("a later table checksum");
    commands.remove(apic_at);
    let script: Vec<u8> = commands.iter().flat_map(|c| c.encode().unwrap()).collect();
    files[2].1 = script;
    let installed = edk2_install(
        &files,
        &[(RSDP_FILE, 0x4000_0000), (TABLES_FILE, 0x4001_0000)],
    )
    .unwrap();
    assert_eq!(
        installed.tables.len(),
        6,
        "EDK2 skips a table whose bytes do not sum to zero"
    );
}
