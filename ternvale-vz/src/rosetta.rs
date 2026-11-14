//! Linux Rosetta share: availability, consent-gated install, guest constants.
//!
//! Apple docs: “Running Intel Binaries in Linux VMs”
//! <https://developer.apple.com/documentation/virtualization/running_intel_binaries_in_linux_vms_with_rosetta>
//! (`VZLinuxRosettaDirectoryShare`, macOS 13.0). Ternvale never redistributes
//! Rosetta.

use crate::error::VzError;
use crate::ffi;

/// Virtiofs tag attached for `rosetta = true`. Validated with
/// `VZVirtioFileSystemDeviceConfiguration.validateTag`.
pub const FS_TAG: &str = "rosetta";

/// Guest mount point documented for that tag.
pub const GUEST_MOUNT: &str = "/mnt/rosetta";

/// Abstract-socket AOT cache name (`VZLinuxRosettaDirectoryShare.CachingOptions`,
/// macOS 14.0). TODO(verify) guest-side daemon wiring for unix-socket mode.
pub const AOT_ABSTRACT_NAME: &str = "ternvale-rosetta";

/// `update-binfmts` interpreter after the share is mounted.
pub const GUEST_RUNTIME: &str = "/mnt/rosetta/rosetta";

/// ELF magic for x86_64 binaries (Apple “Running Intel Binaries in Linux VMs”).
pub const BINFMT_MAGIC: &str =
    r"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00";

/// Mask applied to the first 20 bytes (same Apple page).
pub const BINFMT_MASK: &str =
    r"\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff";

/// `--credentials` from Apple's page.
pub const BINFMT_CREDENTIALS: &str = "yes";

/// `--preserve` on current Apple docs (macOS 14+). Apple: on macOS 13 use `no`.
pub const BINFMT_PRESERVE: &str = "yes";

/// `--fix-binary` from Apple's page.
pub const BINFMT_FIX_BINARY: &str = "yes";

/// `VZLinuxRosettaAvailability` (macOS 13.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Availability {
    /// Host cannot provide Linux Rosetta.
    NotSupported = 0,
    /// Supported but not installed; install only after explicit consent.
    NotInstalled = 1,
    /// Ready to attach `VZLinuxRosettaDirectoryShare`.
    Installed = 2,
}

impl Availability {
    /// Map the C ABI integer.
    #[tracing::instrument(level = "debug", target = "ternvale::rosetta", skip_all, fields(raw))]
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            1 => Self::NotInstalled,
            2 => Self::Installed,
            _ => Self::NotSupported,
        }
    }

    /// Stable name for logs and doctor.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotSupported => "not_supported",
            Self::NotInstalled => "not_installed",
            Self::Installed => "installed",
        }
    }
}

/// Only the exact word `yes` (any case, trimmed) is consent.
#[tracing::instrument(level = "debug", target = "ternvale::rosetta", skip_all)]
pub fn user_consented(answer: &str) -> bool {
    let ok = answer.trim().eq_ignore_ascii_case("yes");
    tracing::debug!(target: "ternvale::rosetta", consented = ok, "rosetta install consent");
    ok
}

/// Test-only override: `not_supported` | `not_installed` | `installed`.
/// Never used to skip consent; only to pretend the host has no Rosetta.
const AVAIL_OVERRIDE: &str = "TERNVALE_ROSETTA_AVAILABILITY";

/// When `1`, `installRosetta` is not called even after `yes` (consent tests).
const INSTALL_SIMULATE: &str = "TERNVALE_ROSETTA_INSTALL_SIMULATE";

/// `VZLinuxRosettaDirectoryShare.availability` (macOS 13.0).
#[tracing::instrument(level = "debug", target = "ternvale::rosetta", skip_all)]
pub fn availability() -> Availability {
    if let Ok(raw) = std::env::var(AVAIL_OVERRIDE) {
        let value = match raw.trim() {
            "not_installed" => Availability::NotInstalled,
            "not_supported" => Availability::NotSupported,
            "installed" => Availability::Installed,
            other => {
                tracing::warn!(
                    target: "ternvale::rosetta",
                    override = %other,
                    "unknown TERNVALE_ROSETTA_AVAILABILITY; using framework"
                );
                Availability::from_raw(ffi::rosetta_availability())
            }
        };
        tracing::warn!(
            target: "ternvale::rosetta",
            availability = value.as_str(),
            "simulated rosetta availability (test override)"
        );
        return value;
    }
    let raw = ffi::rosetta_availability();
    let value = Availability::from_raw(raw);
    tracing::info!(
        target: "ternvale::rosetta",
        availability = value.as_str(),
        raw,
        "vz linux rosetta availability"
    );
    value
}

/// Fail unless the share can be created now. Never calls `installRosetta`.
#[tracing::instrument(level = "debug", target = "ternvale::rosetta", skip_all)]
pub fn require_installed() -> Result<(), VzError> {
    match availability() {
        Availability::Installed => Ok(()),
        Availability::NotInstalled => {
            tracing::warn!(
                target: "ternvale::rosetta",
                "rosetta requested but not installed; refusing silent install"
            );
            Err(VzError::RosettaNotInstalled)
        }
        Availability::NotSupported => {
            tracing::warn!(target: "ternvale::rosetta", "rosetta not supported on this host");
            Err(VzError::RosettaUnavailable)
        }
    }
}

/// `installRosetta` after [`user_consented`]. Never installs when `answer` is not yes.
#[tracing::instrument(
    level = "info",
    target = "ternvale::rosetta",
    skip_all,
    fields(consented)
)]
pub fn install_with_consent(answer: &str) -> Result<(), VzError> {
    let consented = user_consented(answer);
    tracing::Span::current().record("consented", consented);
    if !consented {
        tracing::info!(
            target: "ternvale::rosetta",
            "rosetta install declined; nothing downloaded"
        );
        return Err(VzError::RosettaInstallDeclined);
    }
    if std::env::var(INSTALL_SIMULATE).as_deref() == Ok("1") {
        tracing::info!(
            target: "ternvale::rosetta",
            "TERNVALE_ROSETTA_INSTALL_SIMULATE=1; not calling installRosetta"
        );
        return Ok(());
    }
    tracing::info!(
        target: "ternvale::rosetta",
        "calling VZLinuxRosettaDirectoryShare.installRosetta after consent"
    );
    ffi::rosetta_install()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_yes_is_consent() {
        assert!(user_consented("yes"));
        assert!(user_consented(" YES \n"));
        assert!(!user_consented("y"));
        assert!(!user_consented(""));
        assert!(!user_consented("install"));
    }

    #[test]
    fn declined_install_does_not_call_the_framework() {
        let err = install_with_consent("no").expect_err("declined");
        assert!(matches!(err, VzError::RosettaInstallDeclined), "{err}");
    }

    #[test]
    fn availability_override_simulates_not_installed() {
        // SAFETY: this test is the only thread that reads the override.
        unsafe {
            std::env::set_var(AVAIL_OVERRIDE, "not_installed");
        }
        let value = availability();
        // SAFETY: restore process env after the simulated probe.
        unsafe {
            std::env::remove_var(AVAIL_OVERRIDE);
        }
        assert_eq!(value, Availability::NotInstalled);
    }

    #[test]
    fn simulate_install_still_requires_yes() {
        // SAFETY: serial unit test; simulate flag is restored below.
        unsafe {
            std::env::set_var(INSTALL_SIMULATE, "1");
        }
        let declined = install_with_consent("nope");
        let ok = install_with_consent("yes");
        // SAFETY: clear the simulate flag so later tests call the real path.
        unsafe {
            std::env::remove_var(INSTALL_SIMULATE);
        }
        assert!(matches!(declined, Err(VzError::RosettaInstallDeclined)));
        assert!(ok.is_ok(), "{ok:?}");
    }

    #[test]
    fn apple_binfmt_bytes_are_20_octets() {
        fn binfmt_octets(s: &str) -> usize {
            let mut n = 0;
            let bytes = s.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1] == b'x' {
                    n += 1;
                    i += 4;
                } else {
                    n += 1;
                    i += 1;
                }
            }
            n
        }
        assert_eq!(binfmt_octets(BINFMT_MAGIC), 20, "{BINFMT_MAGIC}");
        assert_eq!(binfmt_octets(BINFMT_MASK), 20, "{BINFMT_MASK}");
        assert!(BINFMT_MAGIC.starts_with(r"\x7fELF"));
        assert_eq!(BINFMT_CREDENTIALS, "yes");
        assert_eq!(BINFMT_FIX_BINARY, "yes");
    }

    #[test]
    fn availability_raw_matches_apple_enum() {
        assert_eq!(Availability::from_raw(0), Availability::NotSupported);
        assert_eq!(Availability::from_raw(1), Availability::NotInstalled);
        assert_eq!(Availability::from_raw(2), Availability::Installed);
        assert_eq!(Availability::from_raw(99), Availability::NotSupported);
    }

    #[test]
    fn linux_plan_blob_carries_the_documented_tag() {
        use std::path::PathBuf;
        use ternvale_config::{BackendKind, Nic, Profile, TpmSection, VmConfig};

        use crate::plan::VzPlan;

        let mut config = VmConfig {
            name: "vz-linux".into(),
            profile: Profile::Linux,
            backend: BackendKind::Vz,
            cpus: 2,
            ram_mib: 256,
            ram_min_mib: None,
            kernel: PathBuf::from("/k"),
            initrd: None,
            cmdline: "console=hvc0".into(),
            boot_disk: false,
            disks: Vec::new(),
            iso: None,
            nics: vec![Nic::user()],
            serial_log: PathBuf::from("/tmp/s.log"),
            firmware: None,
            nvram: None,
            firmware_tables: None,
            vsock: None,
            tpm: Some(TpmSection {
                enabled: false,
                ..TpmSection::default()
            }),
            usb: None,
            audio: None,
            shares: Vec::new(),
            power: Default::default(),
            encryption: false,
            rosetta: true,
            x86_emulation: Default::default(),
            agent: Default::default(),
        };
        let plan = VzPlan::from_config(&config).expect("linux rosetta");
        assert!(plan.rosetta);
        config.rosetta = false;
        config.x86_emulation = ternvale_config::X86Emulation::Auto;
        let auto = VzPlan::from_config(&config).expect("linux auto rosetta");
        assert!(auto.rosetta);
        config.x86_emulation = ternvale_config::X86Emulation::Fex;
        let fex = VzPlan::from_config(&config).expect("linux fex");
        assert!(!fex.rosetta);
        let blob = plan.devices_blob();
        assert!(blob.contains("rosetta=1"), "{blob}");
        assert!(blob.contains("rosetta_tag=rosetta"), "{blob}");
        assert!(blob.contains("rosetta_aot=1"), "{blob}");
        config.rosetta = true;
        config.x86_emulation = Default::default();
        config.profile = Profile::Macos;
        assert!(matches!(
            VzPlan::from_config(&config),
            Err(crate::VzError::UnsupportedOption {
                capability: "rosetta",
                ..
            })
        ));
    }
}
