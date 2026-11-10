//! Resolve `backend = "auto"` from the guest profile and TPM.

use ternvale_config::{BackendKind, Profile, VmConfig};

use crate::BackendError;

/// Outcome of [`select`]: `resolved` is never [`BackendKind::Auto`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Value from the config document.
    pub requested: BackendKind,
    /// Native or VZ after the auto rules.
    pub resolved: BackendKind,
    /// Human-readable reasons logged at INFO.
    pub reasons: Vec<String>,
}

/// Choose a backend and log the decision at INFO on `ternvale::backend`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::backend",
    skip_all,
    fields(vm = %config.name, requested = %config.backend, profile = %config.profile)
)]
pub fn select(config: &VmConfig) -> Result<Selection, BackendError> {
    let selection = resolve(config);
    tracing::info!(
        target: "ternvale::backend",
        vm = %config.name,
        requested = %selection.requested,
        resolved = %selection.resolved,
        profile = %config.profile,
        tpm = config.tpm_enabled(),
        reasons = %selection.reasons.join("; "),
        "backend selected"
    );
    Ok(selection)
}

/// Same rules as [`select`] without the INFO log (listing VMs).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::backend",
    skip_all,
    fields(vm = %config.name, requested = %config.backend)
)]
pub fn resolve(config: &VmConfig) -> Selection {
    let requested = config.backend;
    let mut reasons = Vec::new();
    let resolved = match requested {
        BackendKind::Native => {
            reasons.push("config backend=native".into());
            BackendKind::Native
        }
        BackendKind::Vz => {
            reasons.push("config backend=vz".into());
            BackendKind::Vz
        }
        BackendKind::Auto => auto_choice(config, &mut reasons),
    };
    Selection {
        requested,
        resolved,
        reasons,
    }
}

fn auto_choice(config: &VmConfig, reasons: &mut Vec<String>) -> BackendKind {
    reasons.push(format!("profile={}", config.profile));
    if config.profile == Profile::Macos {
        reasons.push("macOS guest -> vz".into());
        return BackendKind::Vz;
    }
    if config.profile == Profile::Windows {
        reasons.push("Windows guest -> native".into());
        return BackendKind::Native;
    }
    if config.tpm_enabled() {
        reasons.push("tpm enabled -> native".into());
        return BackendKind::Native;
    }
    reasons.push("default -> native".into());
    BackendKind::Native
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ternvale_config::{BackendKind, Profile, TpmSection, VmConfig};

    use super::{resolve, select};

    fn cfg(profile: Profile, backend: BackendKind, tpm: Option<bool>) -> VmConfig {
        VmConfig {
            name: "t".into(),
            profile,
            backend,
            cpus: 1,
            ram_mib: 256,
            ram_min_mib: None,
            kernel: PathBuf::from("/k"),
            initrd: None,
            cmdline: String::new(),
            boot_disk: false,
            disks: Vec::new(),
            iso: None,
            nics: Vec::new(),
            serial_log: PathBuf::from("/tmp/s.log"),
            firmware: None,
            nvram: None,
            firmware_tables: None,
            vsock: None,
            tpm: tpm.map(|enabled| TpmSection {
                enabled,
                ..TpmSection::default()
            }),
            usb: None,
            audio: None,
            shares: Vec::new(),
            power: Default::default(),
            encryption: false,
            agent: Default::default(),
        }
    }

    #[test]
    fn auto_macos_chooses_vz() {
        let sel = resolve(&cfg(Profile::Macos, BackendKind::Auto, None));
        assert_eq!(sel.resolved, BackendKind::Vz);
        assert!(sel.reasons.iter().any(|r| r.contains("macOS")));
    }

    #[test]
    fn auto_windows_and_tpm_choose_native() {
        let win = resolve(&cfg(Profile::Windows, BackendKind::Auto, None));
        assert_eq!(win.resolved, BackendKind::Native);
        let tpm = resolve(&cfg(Profile::Linux, BackendKind::Auto, None));
        assert_eq!(tpm.resolved, BackendKind::Native);
        assert!(tpm.reasons.iter().any(|r| r.contains("tpm")));
        let no_tpm = resolve(&cfg(Profile::Linux, BackendKind::Auto, Some(false)));
        assert_eq!(no_tpm.resolved, BackendKind::Native);
        assert!(no_tpm.reasons.iter().any(|r| r.contains("default")));
    }

    #[test]
    fn explicit_vz_is_honoured() {
        let sel = select(&cfg(Profile::Linux, BackendKind::Vz, Some(false))).expect("select");
        assert_eq!(sel.resolved, BackendKind::Vz);
        assert_eq!(sel.requested, BackendKind::Vz);
    }
}
