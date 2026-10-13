use std::path::{Path, PathBuf};

use ternvale_tpm::{SwtpmError, SwtpmInfo};

use super::{check_swtpm, need_for, Need, NAME};
use crate::doctor::Outcome;

fn found() -> Result<SwtpmInfo, SwtpmError> {
    Ok(SwtpmInfo {
        path: PathBuf::from("/opt/homebrew/bin/swtpm"),
        version: "0.10.2".into(),
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
    std::fs::write(dir.join("bad.toml"), "name = ").expect("write");
    assert_eq!(need_for(&dir.join("bad.toml")), None);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}
