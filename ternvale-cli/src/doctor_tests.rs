use std::path::PathBuf;

use ternvale_hv::MacosVersion;

use super::probe::{disk_facts, log_dir_facts};
use super::{
    check_disk, check_entitlement, check_gic, check_hypervisor, check_log_dir, check_macos,
    check_vz_backend, evaluate, has_hv_entitlement, human, render, DiskFacts, Facts, LogDirFacts,
    Outcome, VmProbe,
};

const SIGNED_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/><key>com.apple.security.virtualization</key><true/></dict></plist>"#;

fn version(major: u32, minor: u32) -> MacosVersion {
    MacosVersion {
        major,
        minor,
        text: format!("{major}.{minor}"),
    }
}

fn healthy() -> Facts {
    Facts {
        macos: Ok(version(26, 1)),
        hv_support: Ok(true),
        exe: PathBuf::from("/bin/ternvale"),
        entitlements: Ok(SIGNED_XML.to_string()),
        vm: Ok(()),
        gic: Some(Ok(())),
        disk: Ok(DiskFacts {
            path: PathBuf::from("/logs"),
            free_bytes: 100 << 30,
            total_bytes: 500 << 30,
        }),
        log_dir: Ok(LogDirFacts {
            path: PathBuf::from("/logs"),
            files: 3,
            bytes: 4096,
        }),
        swtpm: Ok(ternvale_tpm::SwtpmInfo {
            path: PathBuf::from("/opt/homebrew/bin/swtpm"),
            version: "0.10.2".into(),
            features: ternvale_tpm::REQUIRED_FEATURES.map(String::from).to_vec(),
        }),
        qemu_img: Ok(PathBuf::from("/opt/homebrew/bin/qemu-img")),
    }
}

#[test]
fn a_healthy_host_passes_everything() {
    let checks = evaluate(&healthy());
    assert!(
        checks.iter().all(|c| c.outcome == Outcome::Pass),
        "{checks:?}"
    );
    let text = render(&checks);
    assert!(text.starts_with("PASS  macOS version"), "{text}");
    assert!(text.contains("macOS 26.1"), "{text}");
    assert!(text.contains("macOS guest space"), "{text}");
    assert!(text.contains("vz backend"), "{text}");
    assert!(
        text.ends_with("10 passed, 0 warnings, 0 failed, 0 skipped"),
        "{text}"
    );
    assert!(!text.contains("fix:"));
}

#[test]
fn old_macos_fails_with_an_update_fix() {
    let check = check_macos(&Ok(version(14, 6)));
    assert_eq!(check.outcome, Outcome::Fail);
    assert!(
        check.detail.contains("14.6 is older than 15.0"),
        "{}",
        check.detail
    );
    assert!(check.fix.expect("fix").contains("Software Update"));
    assert_eq!(check_macos(&Err("nope".into())).outcome, Outcome::Warn);
}

#[test]
fn vz_backend_needs_virtualization_entitlement() {
    let mut facts = healthy();
    facts.entitlements = Ok("<dict><key>com.apple.security.hypervisor</key><true/></dict>".into());
    let check = check_vz_backend(&facts);
    assert_eq!(check.outcome, Outcome::Fail);
    assert!(check.detail.contains("com.apple.security.virtualization"));
    let ok = check_vz_backend(&healthy());
    assert_eq!(ok.outcome, Outcome::Pass);
}

#[test]
fn missing_qemu_img_is_a_warning() {
    use super::check_qemu_img;
    let check = check_qemu_img(&Err("qemu-img not found".into()));
    assert_eq!(check.outcome, Outcome::Warn);
    assert!(check.fix.expect("fix").contains("brew install qemu"));
}

#[test]
fn unsigned_binary_fails_hypervisor_and_entitlement_with_codesign_fix() {
    let mut facts = healthy();
    facts.entitlements = Ok(String::new());
    facts.vm = Err(VmProbe::Denied);
    facts.gic = None;
    let hv = check_hypervisor(&facts);
    assert_eq!(hv.outcome, Outcome::Fail);
    assert!(hv.detail.contains("HV_DENIED"));
    let entitlement = check_entitlement(&facts);
    assert_eq!(entitlement.outcome, Outcome::Fail);
    let fix = entitlement.fix.expect("fix");
    assert!(
        fix.contains("codesign --sign - --force --entitlements entitlements/ternvale.entitlements /bin/ternvale"),
        "{fix}"
    );
    assert_eq!(hv.fix.as_deref(), Some(fix.as_str()));
    assert_eq!(check_gic(&facts.gic).outcome, Outcome::Skip);
    let text = render(&evaluate(&facts));
    assert!(
        text.contains("FAIL  entitlement") && text.contains("/bin/ternvale lacks"),
        "{text}"
    );
    assert!(text.contains("fix: Sign it:"), "{text}");
    assert!(
        text.ends_with("6 passed, 0 warnings, 3 failed, 1 skipped"),
        "{text}"
    );
}

#[test]
fn no_hypervisor_support_is_a_fail_with_a_hardware_fix() {
    let mut facts = healthy();
    facts.hv_support = Ok(false);
    facts.vm = Err(VmProbe::Other("kern.hv_support is 0".into()));
    let check = check_hypervisor(&facts);
    assert_eq!(check.outcome, Outcome::Fail);
    assert!(check.detail.starts_with("kern.hv_support is 0"));
    assert!(check.fix.expect("fix").contains("nested virtualization"));
    facts.hv_support = Ok(true);
    facts.vm = Err(VmProbe::Other("HV_BUSY (0x1)".into()));
    assert!(check_hypervisor(&facts).detail.contains("HV_BUSY"));
}

#[test]
fn gic_failure_names_the_error() {
    let check = check_gic(&Some(Err("HV_UNSUPPORTED (0xfae9400f)".into())));
    assert_eq!(check.outcome, Outcome::Fail);
    assert!(check.detail.contains("HV_UNSUPPORTED"));
}

#[test]
fn entitlement_parsing_handles_both_codesign_formats() {
    assert!(has_hv_entitlement(SIGNED_XML));
    assert!(!has_hv_entitlement(""));
    assert!(!has_hv_entitlement(
        "<dict><key>com.apple.security.hypervisor</key><false/></dict>"
    ));
    assert!(!has_hv_entitlement(
        "<dict><key>com.apple.security.hypervisor</key><false/><key>other</key><true/></dict>"
    ));
    assert!(has_hv_entitlement(
        "[Dict]\n\t[Key] com.apple.security.hypervisor\n\t[Value]\n\t\t[Bool] true\n"
    ));
    assert!(!has_hv_entitlement(
        "[Key] com.apple.security.hypervisor\n[Value]\n[Bool] false\n[Key] x\n[Value]\n[Bool] true\n"
    ));
}

#[test]
fn disk_thresholds() {
    let disk = |free: u64| {
        check_disk(&Ok(DiskFacts {
            path: PathBuf::from("/logs"),
            free_bytes: free,
            total_bytes: 100 << 30,
        }))
    };
    assert_eq!(disk(512 << 20).outcome, Outcome::Fail);
    assert_eq!(disk(2 << 30).outcome, Outcome::Warn);
    assert_eq!(disk(50 << 30).outcome, Outcome::Pass);
    assert_eq!(
        disk(50 << 30).detail,
        "50.0 GiB free of 100.0 GiB on the volume holding /logs"
    );
    assert_eq!(check_disk(&Err("x".into())).outcome, Outcome::Warn);
}

#[test]
fn macos_guest_space_warns_below_eighty_gib() {
    use super::{check_macos_guest_space, require_macos_create_space, MACOS_GUEST_WARN_BYTES};
    let disk = |free: u64| {
        check_macos_guest_space(&Ok(DiskFacts {
            path: PathBuf::from("/vms"),
            free_bytes: free,
            total_bytes: 500 << 30,
        }))
    };
    assert_eq!(disk(20 << 30).outcome, Outcome::Warn);
    assert_eq!(disk(MACOS_GUEST_WARN_BYTES).outcome, Outcome::Pass);
    assert_eq!(
        super::macos_create_need(64 << 30, 16 << 30),
        (64 << 30) + (16 << 30) + super::MACOS_CREATE_OVERHEAD_BYTES
    );
    let err = require_macos_create_space(100, 5000).expect_err("low");
    assert!(err.contains("not enough disk"), "{err}");
    assert!(err.contains("before downloading"), "{err}");
    assert!(require_macos_create_space(5000, 100).is_ok());
}

#[test]
fn big_or_broken_log_dirs() {
    let big = check_log_dir(&Ok(LogDirFacts {
        path: PathBuf::from("/logs"),
        files: 9000,
        bytes: 3 << 30,
    }));
    assert_eq!(big.outcome, Outcome::Warn);
    assert!(big
        .fix
        .expect("fix")
        .contains("find /logs -name 'ternvale-*' -mtime +14 -delete"));
    let broken = check_log_dir(&Err("/logs is not writable: denied".into()));
    assert_eq!(broken.outcome, Outcome::Fail);
}

#[test]
fn probes_measure_a_real_directory() {
    let dir = std::env::temp_dir().join(format!("ternvale-doctor-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear");
    }
    let facts = log_dir_facts(&dir).expect("creates and probes");
    assert_eq!((facts.files, facts.bytes), (0, 0));
    std::fs::write(dir.join("ternvale-x-20261002-120000.log"), b"12345").expect("log");
    let facts = log_dir_facts(&dir).expect("probe");
    assert_eq!((facts.files, facts.bytes), (1, 5));
    let disk = disk_facts(&dir.join("not/yet/created")).expect("ancestor statfs");
    assert!(
        disk.total_bytes > 0 && disk.free_bytes <= disk.total_bytes,
        "{disk:?}"
    );

    let file = dir.join("plain-file");
    std::fs::write(&file, b"x").expect("file");
    let error = log_dir_facts(&file).expect_err("a file is not a directory");
    assert!(error.contains("create"), "{error}");
}

#[test]
fn human_sizes() {
    assert_eq!(human(0), "0 B");
    assert_eq!(human(1023), "1023 B");
    assert_eq!(human(1536), "1.5 KiB");
    assert_eq!(human(5 << 30), "5.0 GiB");
    assert_eq!(human(u64::MAX), "16777216.0 TiB");
}
