//! Map [`VmConfig`] onto a VZ Linux guest and reject unsupported options.

use ternvale_agent_proto::AGENT_PORT;
use ternvale_config::{Profile, VmConfig};

use crate::error::VzError;

/// Devices and paths the Swift host will attach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VzPlan {
    pub name: String,
    pub cpus: u32,
    pub ram_bytes: u64,
    /// Empty: [`VZEFIBootLoader`]. Non-empty: [`VZLinuxBootLoader`].
    pub kernel: String,
    pub initrd: String,
    pub cmdline: String,
    pub nvram: String,
    pub serial_log: String,
    pub uds_dir: String,
    pub listen_ports: Vec<u32>,
    pub disks: Vec<(String, bool)>,
    pub attach_vsock: bool,
    pub attach_nat: bool,
    /// Empty: Linux generic platform. Hex of `VZMacHardwareModel`.
    pub macos_hw_hex: String,
    /// Hex of `VZMacMachineIdentifier`.
    pub macos_id_hex: String,
    /// `VZMacAuxiliaryStorage` path.
    pub macos_aux: String,
}

impl VzPlan {
    /// Validate `config` for a Linux VZ guest.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::vz",
        skip_all,
        fields(vm = %config.name, profile = %config.profile)
    )]
    pub fn from_config(config: &VmConfig) -> Result<Self, VzError> {
        reject_unsupported(config)?;
        let efi = config.firmware.is_some();
        let kernel = if efi {
            String::new()
        } else {
            path_string(&config.kernel)
        };
        let nvram = if efi {
            config
                .nvram_path()
                .ok()
                .flatten()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut disks = Vec::new();
        for disk in &config.disks {
            disks.push((disk.path.display().to_string(), disk.read_only));
        }
        if let Some(iso) = &config.iso {
            for path in [&iso.installer, &iso.tools, &iso.drivers, &iso.unattend]
                .into_iter()
                .flatten()
            {
                disks.push((path.display().to_string(), true));
            }
        }
        let (uds_dir, listen_ports, attach_vsock) = vsock_plan(config);
        let attach_nat = true;
        let (macos_hw_hex, macos_id_hex, macos_aux) = macos_platform(config)?;
        tracing::info!(
            target: "ternvale::vz",
            vm = %config.name,
            efi,
            disks = disks.len(),
            attach_vsock,
            ports = listen_ports.len(),
            "vz linux plan"
        );
        Ok(Self {
            name: config.name.clone(),
            cpus: config.cpus,
            ram_bytes: config.ram_mib.saturating_mul(1024).saturating_mul(1024),
            kernel,
            initrd: config
                .initrd
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            cmdline: if config.profile == Profile::Macos {
                String::new()
            } else {
                with_hvc_console(&config.cmdline)
            },
            nvram,
            serial_log: config.serial_log.display().to_string(),
            uds_dir,
            listen_ports,
            disks,
            attach_vsock,
            attach_nat,
            macos_hw_hex,
            macos_id_hex,
            macos_aux,
        })
    }
}

/// VZ serial is virtio-console (`hvc0`), not the HV PL011 (`ttyAMA0`).
/// HV-style `console=ttyAMA0` / `earlycon=pl011,...` is rewritten so the same
/// guest configs still print on the VZ console.
fn with_hvc_console(cmdline: &str) -> String {
    let mut tokens: Vec<String> = cmdline
        .split_whitespace()
        .filter_map(|token| {
            if token.starts_with("earlycon=pl011") {
                return None;
            }
            if token.starts_with("console=ttyAMA") {
                return Some("console=hvc0".into());
            }
            Some(token.to_string())
        })
        .collect();
    if !tokens.iter().any(|t| t.starts_with("console=")) {
        tokens.push("console=hvc0".into());
    }
    let rewritten = tokens.join(" ");
    if rewritten != cmdline {
        tracing::debug!(
            target: "ternvale::vz",
            original = cmdline,
            cmdline = %rewritten,
            "rewrote kernel cmdline for virtio-console"
        );
    }
    rewritten
}

fn path_string(path: &std::path::Path) -> String {
    path.display().to_string()
}

fn macos_platform(config: &VmConfig) -> Result<(String, String, String), VzError> {
    if config.profile != Profile::Macos {
        return Ok((String::new(), String::new(), String::new()));
    }
    let bundle = config.serial_log.parent().ok_or_else(|| VzError::Host {
        op: "macos_platform",
        detail: "serial_log has no parent directory for the VM bundle".into(),
    })?;
    let path = crate::macos::MacosIdentity::path_in(bundle);
    let identity = crate::macos::MacosIdentity::load(&path)?;
    let aux = crate::macos::MacosIdentity::aux_path_in(bundle);
    if !aux.is_file() {
        return Err(VzError::Host {
            op: "macos_platform",
            detail: format!(
                "{} is missing; the VM cannot boot without AuxiliaryStorage",
                aux.display()
            ),
        });
    }
    Ok((
        identity.hardware_hex(),
        identity.identifier_hex(),
        aux.display().to_string(),
    ))
}

fn vsock_plan(config: &VmConfig) -> (String, Vec<u32>, bool) {
    let Some(section) = &config.vsock else {
        return (String::new(), Vec::new(), false);
    };
    let mut ports = section.listen_ports.clone();
    if section.agent && section.device && !ports.contains(&AGENT_PORT) {
        ports.push(AGENT_PORT);
    }
    let uds = section
        .uds_dir
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    (uds, ports, section.device)
}

fn reject_unsupported(config: &VmConfig) -> Result<(), VzError> {
    if config.profile == Profile::Windows {
        return Err(unsupported("profile=windows", "guest_windows"));
    }
    if config.tpm_enabled() {
        return Err(unsupported("[tpm]", "tpm"));
    }
    if config
        .usb
        .as_ref()
        .is_some_and(|usb| !usb.passthrough.is_empty())
    {
        return Err(unsupported("[usb].passthrough", "usb_passthrough"));
    }
    if let Some(min) = config.ram_min_mib {
        if min < config.ram_mib {
            return Err(unsupported("ram_min_mib", "balloon"));
        }
    }
    if !config.shares.is_empty() {
        return Err(unsupported("[[shares]]", "shared_folders"));
    }
    if config.encryption {
        return Err(unsupported("encryption", "none"));
    }
    for (index, nic) in config.nics.iter().enumerate() {
        if nic.backend != "user" {
            return Err(unsupported(
                format!("nics[{index}].backend={}", nic.backend),
                "bridged_net",
            ));
        }
    }
    Ok(())
}

fn unsupported(option: impl Into<String>, capability: &'static str) -> VzError {
    let option = option.into();
    tracing::warn!(
        target: "ternvale::vz",
        option = %option,
        capability,
        "vz rejected unsupported option"
    );
    VzError::UnsupportedOption { option, capability }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ternvale_config::{
        BackendKind, Nic, Profile, Share, TpmSection, UsbPassthroughEntry, UsbSection, VmConfig,
    };

    use super::VzPlan;
    use crate::VzError;

    fn linux() -> VmConfig {
        VmConfig {
            name: "vz-linux".into(),
            profile: Profile::Linux,
            backend: BackendKind::Vz,
            cpus: 2,
            ram_mib: 256,
            ram_min_mib: None,
            kernel: PathBuf::from("/k"),
            initrd: None,
            cmdline: "console=ttyAMA0".into(),
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
            agent: Default::default(),
        }
    }

    #[test]
    fn linux_kernel_plan() {
        let plan = VzPlan::from_config(&linux()).expect("plan");
        assert_eq!(plan.kernel, "/k");
        assert!(plan.attach_nat);
        assert_eq!(plan.cpus, 2);
        assert_eq!(plan.ram_bytes, 256 * 1024 * 1024);
        assert!(plan.cmdline.contains("console=hvc0"), "{}", plan.cmdline);
        assert!(
            !plan.cmdline.contains("ttyAMA"),
            "pl011 console rewritten: {}",
            plan.cmdline
        );
    }

    #[test]
    fn windows_with_tpm_names_a_capability() {
        let mut config = linux();
        config.profile = Profile::Windows;
        config.tpm = Some(TpmSection {
            enabled: true,
            ..TpmSection::default()
        });
        match VzPlan::from_config(&config) {
            Err(VzError::UnsupportedOption { option, capability }) => {
                assert!(
                    capability == "guest_windows" || capability == "tpm",
                    "{option} {capability}"
                );
                assert!(
                    option.contains("windows") || option.contains("tpm"),
                    "{option}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn efi_clears_kernel() {
        let mut config = linux();
        config.firmware = Some(PathBuf::from("/fw.fd"));
        let plan = VzPlan::from_config(&config).expect("efi");
        assert!(plan.kernel.is_empty());
    }

    #[test]
    fn macos_without_identity_cannot_boot() {
        let mut config = linux();
        config.profile = Profile::Macos;
        config.serial_log = PathBuf::from("/tmp/no-such-tv-macos/serial.log");
        let err = VzPlan::from_config(&config).expect_err("identity");
        let text = err.to_string();
        assert!(
            text.contains("macos-platform")
                || text.contains("cannot boot")
                || text.contains("read:"),
            "{text}"
        );
    }

    #[test]
    fn tpm_names_capability() {
        let mut config = linux();
        config.tpm = None;
        match VzPlan::from_config(&config) {
            Err(VzError::UnsupportedOption {
                option,
                capability: "tpm",
            }) => assert!(option.contains("tpm"), "{option}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn windows_pci_profile_rejected() {
        let mut config = linux();
        config.profile = Profile::Windows;
        match VzPlan::from_config(&config) {
            Err(VzError::UnsupportedOption {
                capability: "guest_windows",
                ..
            }) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn balloon_usb_share_vmnet_encryption() {
        let mut config = linux();
        config.ram_min_mib = Some(128);
        assert!(matches!(
            VzPlan::from_config(&config),
            Err(VzError::UnsupportedOption {
                capability: "balloon",
                ..
            })
        ));
        config = linux();
        config.usb = Some(UsbSection {
            enabled: true,
            passthrough: vec![UsbPassthroughEntry {
                vendor_id: 1,
                product_id: 2,
                port: None,
            }],
        });
        assert!(matches!(
            VzPlan::from_config(&config),
            Err(VzError::UnsupportedOption {
                capability: "usb_passthrough",
                ..
            })
        ));
        config = linux();
        config.shares = vec![Share {
            tag: "host".into(),
            host_path: PathBuf::from("/tmp"),
            mount_point: "/mnt/t".into(),
            read_only: false,
        }];
        assert!(matches!(
            VzPlan::from_config(&config),
            Err(VzError::UnsupportedOption {
                capability: "shared_folders",
                ..
            })
        ));
        config = linux();
        config.nics[0].backend = "vmnet".into();
        assert!(matches!(
            VzPlan::from_config(&config),
            Err(VzError::UnsupportedOption {
                capability: "bridged_net",
                ..
            })
        ));
        config = linux();
        config.encryption = true;
        assert!(matches!(
            VzPlan::from_config(&config),
            Err(VzError::UnsupportedOption {
                capability: "none",
                ..
            })
        ));
    }
}
