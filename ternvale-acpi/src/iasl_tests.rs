//! Runs ACPICA `iasl` on the generated tables.
//!
//! `iasl -d` disassembles each table; any `Warning` or `Error` line fails the
//! test (iasl exits 0 even for a bad checksum, so the exit code alone is not
//! enough). The DSDT disassembly is then recompiled with optimizations off
//! (`-oa`) and its AML must match ours byte for byte, because `iasl -d` alone
//! accepts some malformed AML (a PkgLength past the end of the table).
//!
//! Skipped when `iasl` is not on PATH; set `TERNVALE_REQUIRE_IASL=1` to make
//! that a failure instead (`brew install acpica`). `TERNVALE_IASL` names a
//! different binary.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::sdt::{checksum, SDT_CHECKSUM_OFFSET, SDT_HEADER_LEN};
use crate::{dsdt, AcpiTables, PsciConduit};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

/// False (skip) when iasl is missing and not required.
fn have_iasl() -> bool {
    if Command::new(iasl_bin()).arg("-v").output().is_ok() {
        return true;
    }
    let required = std::env::var("TERNVALE_REQUIRE_IASL").is_ok_and(|v| v == "1");
    assert!(
        !required,
        "TERNVALE_REQUIRE_IASL=1 but {} does not run",
        iasl_bin()
    );
    false
}

fn iasl_bin() -> String {
    std::env::var("TERNVALE_IASL").unwrap_or_else(|_| "iasl".to_string())
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ternvale-acpi-iasl-{}-{}-{name}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Run iasl in `dir`; returns (exit ok, stdout + stderr).
fn iasl(dir: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(iasl_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run iasl");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

/// Lines where iasl reports a problem.
fn problems(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| !line.starts_with("Compilation successful"))
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("warning") || lower.contains("error") || lower.contains("incorrect")
        })
        .collect()
}

/// Write `bytes` to `<dir>/<stem>.dat`, run `iasl -d`, and return the
/// disassembly and iasl's output.
fn disassemble(dir: &Path, stem: &str, bytes: &[u8]) -> (String, String) {
    let input = format!("{stem}.dat");
    std::fs::write(dir.join(&input), bytes).expect("write table");
    let (ok, text) = iasl(dir, &["-d", &input]);
    assert!(ok, "iasl -d {input} failed:\n{text}");
    // AML tables end with "Disassembly completed", data tables with "... decoded".
    assert!(
        text.contains("Disassembly completed") || text.contains("] decoded"),
        "{text}"
    );
    let dsl = std::fs::read_to_string(dir.join(format!("{stem}.dsl"))).expect("read .dsl");
    (dsl, text)
}

/// Recompile `<stem>.dsl` without optimizations; returns the AML and iasl's output.
fn recompile(dir: &Path, stem: &str) -> (Vec<u8>, String) {
    let (ok, text) = iasl(dir, &["-oa", "-p", "roundtrip", &format!("{stem}.dsl")]);
    assert!(ok, "iasl -oa {stem}.dsl failed:\n{text}");
    let aml = std::fs::read(dir.join("roundtrip.aml")).expect("read roundtrip.aml");
    (aml, text)
}

fn dsdt_with_body(body: &[u8]) -> Vec<u8> {
    let mut table = dsdt().expect("dsdt")[..SDT_HEADER_LEN].to_vec();
    table.extend_from_slice(body);
    let len = table.len() as u32;
    table[4..8].copy_from_slice(&len.to_le_bytes());
    table[SDT_CHECKSUM_OFFSET] = 0;
    table[SDT_CHECKSUM_OFFSET] = checksum(&table);
    table
}

#[test]
fn dsdt_disassembles_cleanly_and_recompiles_to_the_same_aml() {
    if !have_iasl() {
        return;
    }
    let dir = scratch("dsdt");
    let ours = dsdt().expect("dsdt");
    let (dsl, text) = disassemble(&dir, "dsdt", &ours);
    assert_eq!(problems(&text), Vec::<&str>::new(), "{text}");
    assert!(
        dsl.contains("DefinitionBlock (\"\", \"DSDT\", 2, \"TERNVL\", \"TERNDSDT\", 0x00000001)"),
        "{dsl}"
    );
    assert!(dsl.contains("Scope (\\_SB)"), "{dsl}");

    let (aml, text) = recompile(&dir, "dsdt");
    assert_eq!(problems(&text), Vec::<&str>::new(), "{text}");
    assert!(text.contains("0 Errors, 0 Warnings"), "{text}");
    assert_eq!(&aml[SDT_HEADER_LEN..], &ours[SDT_HEADER_LEN..], "AML body");
    assert_eq!(&aml[..9], &ours[..9], "signature, length, revision");
    std::fs::remove_dir_all(&dir).expect("remove scratch");
}

#[test]
fn xsdt_and_fadt_disassemble_cleanly() {
    if !have_iasl() {
        return;
    }
    let dir = scratch("data");
    let tables = AcpiTables::build(0x0910_0000, 0x2_0000, PsciConduit::Hvc).expect("build");
    // iasl 20260408 cannot take a standalone RSDP: it guesses "CDAT" and stops
    // (QEMU's own RSDP does the same). checksum_tests and rsdp.rs cover it.
    let expect: [(&str, &[&str]); 2] = [
        (
            "XSDT",
            &["\"XSDT\"", "ACPI Table Address 0 : 0000000009100058"],
        ),
        (
            "FACP",
            &[
                "\"FACP\"",
                "Hardware Reduced (V5) : 1",
                "PSCI Compliant : 1",
                "Must use HVC for PSCI : 1",
                "FADT Minor Revision : 05",
                "Table Length : 00000114",
                "[08Ch 0140 008h] DSDT Address : 0000000009100170",
            ],
        ),
    ];
    for (signature, needles) in expect {
        let table = tables
            .tables()
            .iter()
            .find(|t| t.signature == signature)
            .expect("table");
        let stem = signature.trim_end().replace(' ', "_").to_ascii_lowercase();
        let (dsl, text) = disassemble(&dir, &stem, &table.bytes);
        assert_eq!(problems(&text), Vec::<&str>::new(), "{signature}: {text}");
        assert_eq!(problems(&dsl), Vec::<&str>::new(), "{signature}: {dsl}");
        let squeezed = squeeze(&dsl);
        for needle in needles {
            assert!(
                squeezed.contains(needle),
                "{signature}: {needle:?} missing in\n{dsl}"
            );
        }
    }
    std::fs::remove_dir_all(&dir).expect("remove scratch");
}

/// Collapse runs of spaces so field lines compare without iasl's column padding.
fn squeeze(text: &str) -> String {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_iasl_checks_catch_broken_tables() {
    if !have_iasl() {
        return;
    }
    let dir = scratch("broken");
    let mut bad_sum = dsdt().expect("dsdt");
    bad_sum[SDT_CHECKSUM_OFFSET] ^= 0x55;
    let (_, text) = disassemble(&dir, "badsum", &bad_sum);
    assert!(
        !problems(&text).is_empty(),
        "bad checksum not flagged:\n{text}"
    );

    let bad_name = dsdt_with_body(&[0x10, 0x06, b'\\', b'a', b'b', b'_', b'_']);
    let (_, text) = disassemble(&dir, "badname", &bad_name);
    assert!(
        !problems(&text).is_empty(),
        "lowercase name not flagged:\n{text}"
    );

    // PkgLength 0x20 runs past the end; iasl -d stays quiet, the round trip does not match.
    let truncated = dsdt_with_body(&[0x10, 0x20, b'\\', b'_', b'S', b'B', b'_']);
    disassemble(&dir, "truncated", &truncated);
    let (aml, _) = recompile(&dir, "truncated");
    assert_ne!(&aml[SDT_HEADER_LEN..], &truncated[SDT_HEADER_LEN..]);
    std::fs::remove_dir_all(&dir).expect("remove scratch");
}
