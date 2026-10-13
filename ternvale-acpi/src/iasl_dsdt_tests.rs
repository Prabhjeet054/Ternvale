//! ACPICA checks on the DSDT (`\_SB.PCI0` and `\_SB.TPM2`).
//!
//! 1. `iasl -d` disassembles it with no warning, error, or "incorrect" line,
//!    and the namespace objects come out as written.
//! 2. The disassembly recompiles (`iasl -oa`, optimizations off) to the same
//!    AML, which catches malformed AML `iasl -d` alone accepts.
//! 3. A hand-written ASL `PCI0` and `TPM2`, built from the spec's ASL macros
//!    (`WordBusNumber`, `DWordMemory`, `Memory32Fixed`, `EisaId`) and an
//!    independently written INTx swizzle, compiles to exactly our AML. iasl 20260408 never turns a
//!    buffer back into a `ResourceTemplate` (even its own output), so this is
//!    what checks the `_CRS` descriptors field by field.
//!
//! Skipped without iasl unless `TERNVALE_REQUIRE_IASL=1` (see `iasl_tests.rs`).

use std::fmt::Write;

use crate::config::tests::sample;
use crate::dsdt;
use crate::iasl_tests::{disassemble, have_iasl, iasl, problems, recompile, scratch, squeeze};
use crate::sdt::SDT_HEADER_LEN;

#[test]
fn dsdt_disassembles_cleanly_and_recompiles_to_the_same_aml() {
    if !have_iasl() {
        return;
    }
    let dir = scratch("dsdt");
    let ours = dsdt(&sample(1)).expect("dsdt");
    let (dsl, text) = disassemble(&dir, "dsdt", &ours);
    assert_eq!(problems(&text), Vec::<&str>::new(), "{text}");
    let squeezed = squeeze(&dsl);
    for needle in [
        "DefinitionBlock (\"\", \"DSDT\", 2, \"TERNVL\", \"TERNDSDT\", 0x00000001)",
        "Scope (\\_SB)",
        "Device (PCI0)",
        "Name (_HID, EisaId (\"PNP0A08\") /* PCI Express Bus */)",
        "Name (_CID, EisaId (\"PNP0A03\") /* PCI Bus */)",
        "Name (_SEG, Zero)",
        "Name (_BBN, Zero)",
        "Name (_UID, Zero)",
        "Name (_CCA, One)",
        "Name (_PRT, Package (0x80)",
        "Name (_CRS, Buffer (0x2C)",
        "Device (RES0)",
        "Name (_HID, EisaId (\"PNP0C02\") /* PNP Motherboard Resources */)",
        "Name (_CRS, Buffer (0x1C)",
        "Device (TPM2)",
        "Name (_HID, \"MSFT0101\" /* TPM 2.0 Security Device */)",
        "Name (_CRS, Buffer (0x0E)",
    ] {
        assert!(squeezed.contains(needle), "{needle:?} missing in\n{dsl}");
    }
    assert_eq!(squeezed.matches("Package (0x04)").count(), 128, "{dsl}");

    let (aml, text) = recompile(&dir, "dsdt");
    assert_eq!(problems(&text), Vec::<&str>::new(), "{text}");
    assert!(text.contains("0 Errors, 0 Warnings"), "{text}");
    assert_eq!(&aml[SDT_HEADER_LEN..], &ours[SDT_HEADER_LEN..], "AML body");
    assert_eq!(&aml[..9], &ours[..9], "signature, length, revision");
    std::fs::remove_dir_all(&dir).expect("remove scratch");
}

/// iasl's namespace listing (`-ln`): `TPM2` is a direct child of `\_SB` with
/// exactly `_HID`, `_UID` and `_CRS`, the object `tpm2.rs` builds from the
/// TCG ACPI Specification's TPM 2.0 device object section.
#[test]
fn dsdt_namespace_has_sb_tpm2_with_hid_uid_crs() {
    if !have_iasl() {
        return;
    }
    let dir = scratch("dsdt-ns");
    disassemble(&dir, "dsdt", &dsdt(&sample(1)).expect("dsdt"));
    let (ok, text) = iasl(&dir, &["-ln", "-p", "ns", "dsdt.dsl"]);
    assert!(ok && text.contains("0 Errors, 0 Warnings"), "{text}");
    let listing = std::fs::read_to_string(dir.join("ns.nsp")).expect("read ns.nsp");
    let tpm: Vec<Vec<&str>> = listing
        .lines()
        .filter(|line| line.starts_with("\\_SB_.TPM2"))
        .map(|line| line.split_whitespace().take(2).collect())
        .collect();
    assert_eq!(
        tpm,
        [
            ["\\_SB_.TPM2", "Device"],
            ["\\_SB_.TPM2._HID", "String"],
            ["\\_SB_.TPM2._UID", "Integer"],
            ["\\_SB_.TPM2._CRS", "Buffer"],
        ],
        "{listing}"
    );
    let squeezed = squeeze(&listing);
    for needle in [
        "[2] TPM2 - Device",
        "[3] _HID - String [Initial Value \"MSFT0101\"]",
        "[3] _UID - Integer [Initial Value 0x0000000000000000]",
        "[3] _CRS - Buffer [Initial Length 0x0E bytes]",
    ] {
        assert!(
            squeezed.contains(needle),
            "{needle:?} missing in\n{listing}"
        );
    }
    std::fs::remove_dir_all(&dir).expect("remove scratch");
}

#[test]
fn hand_written_asl_pci0_compiles_to_our_aml() {
    if !have_iasl() {
        return;
    }
    let dir = scratch("pci0-asl");
    std::fs::write(dir.join("pci0.asl"), reference_asl()).expect("write asl");
    let (ok, text) = iasl(&dir, &["-oa", "pci0.asl"]);
    assert!(ok, "iasl pci0.asl failed:\n{text}");
    assert_eq!(problems(&text), Vec::<&str>::new(), "{text}");
    assert!(text.contains("0 Errors, 0 Warnings"), "{text}");
    let theirs = std::fs::read(dir.join("pci0.aml")).expect("read pci0.aml");
    let ours = dsdt(&sample(1)).expect("dsdt");
    assert_eq!(
        &theirs[SDT_HEADER_LEN..],
        &ours[SDT_HEADER_LEN..],
        "iasl's AML for the hand-written PCI0 differs from the builder's"
    );
    std::fs::remove_dir_all(&dir).expect("remove scratch");
}

/// `\_SB.PCI0` for `sample(1)` (ECAM 0x3f000000 buses 0-15, MMIO
/// 0x10000000+0x2f000000, INTx SPIs 3-6 = GSIVs 35-38) and `\_SB.TPM2` (CRB
/// 0x09140000+0x5000) in ASL. `Zero` and
/// `One` are spelled out because `-oa` keeps a literal `0` as `BytePrefix`.
fn reference_asl() -> String {
    let int = |value: u32| match value {
        0 => "Zero".to_string(),
        1 => "One".to_string(),
        _ => format!("{value:#x}"),
    };
    let mut prt = String::new();
    for device in 0u32..32 {
        for pin in 0u32..4 {
            let address = (device << 16) | 0xffff;
            let gsiv = 32 + 3 + (device + pin) % 4;
            writeln!(
                prt,
                "Package () {{ {address:#x}, {}, Zero, {gsiv:#x} }},",
                int(pin)
            )
            .expect("write to a String");
        }
    }
    format!(
        r#"DefinitionBlock ("", "DSDT", 2, "TERNVL", "TERNDSDT", 0x00000001)
{{
    Scope (\_SB)
    {{
        Device (PCI0)
        {{
            Name (_HID, EisaId ("PNP0A08"))
            Name (_CID, EisaId ("PNP0A03"))
            Name (_SEG, Zero)
            Name (_BBN, Zero)
            Name (_UID, Zero)
            Name (_CCA, One)
            Name (_PRT, Package () {{
{prt}            }})
            Name (_CRS, ResourceTemplate ()
            {{
                WordBusNumber (ResourceProducer, MinFixed, MaxFixed, PosDecode,
                    0x0000, 0x0000, 0x000F, 0x0000, 0x0010,,,)
                DWordMemory (ResourceProducer, PosDecode, MinFixed, MaxFixed, NonCacheable, ReadWrite,
                    0x00000000, 0x10000000, 0x3EFFFFFF, 0x00000000, 0x2F000000,,,,
                    AddressRangeMemory, TypeStatic)
            }})
            Device (RES0)
            {{
                Name (_HID, EisaId ("PNP0C02"))
                Name (_CRS, ResourceTemplate ()
                {{
                    DWordMemory (ResourceConsumer, PosDecode, MinFixed, MaxFixed, NonCacheable, ReadWrite,
                        0x00000000, 0x3F000000, 0x3FFFFFFF, 0x00000000, 0x01000000,,,,
                        AddressRangeMemory, TypeStatic)
                }})
            }}
        }}
        Device (TPM2)
        {{
            Name (_HID, "MSFT0101")
            Name (_UID, Zero)
            Name (_CRS, ResourceTemplate ()
            {{
                Memory32Fixed (ReadWrite, 0x09140000, 0x00005000)
            }})
        }}
    }}
}}
"#
    )
}
