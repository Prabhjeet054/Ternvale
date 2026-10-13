use std::path::{Path, PathBuf};

use ternvale_tpm::{SwtpmError, SwtpmInfo};

use super::{check_swtpm, need_for, Need, NAME};
use crate::doctor::Outcome;

fn found() -> Result<SwtpmInfo, SwtpmError> {
    found_version("0.10.2")
}

fn found_version(version: &str) -> Result<SwtpmInfo, SwtpmError> {
    Ok(SwtpmInfo {
        path: PathBuf::from("/opt/homebrew/bin/swtpm"),
        version: version.into(),
        features: Vec::new(),
    })
}

fn missing() -> Result<SwtpmInfo, SwtpmError> {
    Err(SwtpmError::NotFound {
        searched: "swtpm in PATH, /opt/homebrew/bin, /usr/local/bin".into(),
    })
}

#[test]
fn installed_swtpm_passes_with_its_version() {
    let check = check_swtpm(&found(), Need::Optional);
    assert_eq!((check.name.as_str(), check.outcome), (NAME, Outcome::Pass));
    assert_eq!(check.detail, "swtpm 0.10.2 at /opt/homebrew/bin/swtpm");
    assert_eq!(check.fix, None);
}

#[test]
fn an_untested_or_unreadable_version_warns_with_an_upgrade_fix() {
    assert_eq!(
        check_swtpm(&found_version("0.11.0"), Need::Required).outcome,
        Outcome::Pass
    );
    let old = check_swtpm(&found_version("0.9.6"), Need::Required);
    assert_eq!(old.outcome, Outcome::Warn, "usable, so not a failure");
    assert!(
        old.detail
            .starts_with("swtpm 0.9.6 at /opt/homebrew/bin/swtpm has every feature")
            && old
                .detail
                .contains("is older; Ternvale is tested with 0.10 or newer"),
        "{}",
        old.detail
    );
    assert!(old.fix.expect("fix").contains("brew upgrade swtpm"));
    let unknown = check_swtpm(&found_version("unknown"), Need::Optional);
    assert_eq!(unknown.outcome, Outcome::Warn);
    assert!(
        unknown.detail.contains("its version cannot be read"),
        "{}",
        unknown.detail
    );
}

#[test]
fn missing_swtpm_warns_with_install_instructions() {
    let check = check_swtpm(&missing(), Need::Optional);
    assert_eq!(check.outcome, Outcome::Warn);
    assert!(check.detail.contains("Windows 11"), "{}", check.detail);
    let fix = check.fix.expect("fix");
    assert!(
        fix.contains("brew install swtpm") && fix.contains("[tpm] swtpm"),
        "{fix}"
    );
}

#[test]
fn missing_swtpm_fails_when_the_config_requires_it() {
    assert_eq!(
        check_swtpm(&missing(), Need::Required).outcome,
        Outcome::Fail
    );
    let unused = check_swtpm(&missing(), Need::Unused("[tpm] backend = \"stub\""));
    assert_eq!(unused.outcome, Outcome::Skip);
    assert!(unused.detail.contains("stub"), "{}", unused.detail);
}

#[test]
fn old_swtpm_gets_an_upgrade_fix_naming_the_features() {
    let old = Err(SwtpmError::Unsupported {
        path: PathBuf::from("/usr/local/bin/swtpm"),
        reason: "lacks tpmstate-opt-lock".into(),
    });
    let check = check_swtpm(&old, Need::Optional);
    assert_eq!(check.outcome, Outcome::Warn);
    let fix = check.fix.expect("fix");
    assert!(
        fix.contains("brew upgrade swtpm") && fix.contains("ctrl-opt-terminate"),
        "{fix}"
    );
}

fn need(dir: &Path, extra: &str) -> Need {
    let path = dir.join("vm.toml");
    std::fs::write(
        &path,
        format!("name = \"t\"\ncpus = 1\nram_mib = 256\nfirmware = \"/fw.fd\"\nserial_log = \"/tmp/s.log\"\n{extra}"),
    )
    .expect("write");
    need_for(&path).expect("parsed").0
}

#[test]
fn the_vm_config_decides_whether_swtpm_matters() {
    let dir = std::env::temp_dir().join(format!("ternvale-cli-doctor-tpm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    assert_eq!(need(&dir, ""), Need::Optional);
    assert_eq!(need(&dir, "[tpm]\nbackend = \"swtpm\"\n"), Need::Required);
    assert!(matches!(
        need(&dir, "[tpm]\nbackend = \"stub\"\n"),
        Need::Unused(_)
    ));
    assert!(matches!(
        need(&dir, "firmware_tables = \"fdt\"\n"),
        Need::Unused(_)
    ));
    let disabled = need(&dir, "[tpm]\nenabled = false\n");
    assert!(
        matches!(disabled, Need::Unused(why) if why.contains("enabled = false")),
        "{disabled:?}"
    );
    let skipped = check_swtpm(&missing(), disabled);
    assert_eq!(skipped.outcome, Outcome::Skip);
    assert!(
        skipped.detail.contains("the VM has no TPM"),
        "{}",
        skipped.detail
    );
    std::fs::write(dir.join("bad.toml"), "name = ").expect("write");
    assert_eq!(need_for(&dir.join("bad.toml")), None);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}
