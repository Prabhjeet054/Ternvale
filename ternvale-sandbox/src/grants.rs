//! Host paths the sandboxed VMM may touch.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ternvale_config::VmConfig;
use ternvale_log::LogConfig;
use ternvale_net::{socket_path as vmnet_socket, SYSTEM_SOCK};

/// One allowed host path (file or directory).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Grant {
    /// Absolute path.
    pub path: PathBuf,
    /// Directory grants use `(subpath …)`; files use `(literal …)` plus parent write for sockets.
    pub directory: bool,
    /// Seatbelt `file-write*` in addition to `file-read*`.
    pub write: bool,
}

/// Paths collected from `config` plus Ternvale's log/run/helper sockets.
#[derive(Debug, Clone)]
pub struct Grants {
    /// Allow list.
    pub paths: Vec<Grant>,
    /// User-mode NAT needs `network-outbound` / bind.
    pub network_client: bool,
    /// Unix socket to `ternvale-vmnetd`.
    pub helper_sock: PathBuf,
    /// `[audio] input`: Seatbelt `device-microphone` when true.
    pub microphone: bool,
    /// `[camera] enabled`: Seatbelt `device-camera` when true.
    pub camera: bool,
}

impl Grants {
    /// Inventory `config`. `config_path` is the TOML file (VM bundle lives beside it).
    #[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all, fields(vm = %config.name))]
    pub fn from_config(config: &VmConfig, config_path: &Path) -> Self {
        let mut set = BTreeSet::new();
        let mut add = |path: PathBuf, directory: bool, write: bool| {
            if path.as_os_str().is_empty() {
                return;
            }
            let path = std::path::absolute(&path).unwrap_or(path);
            set.insert(Grant {
                path,
                directory,
                write,
            });
        };

        if let Some(parent) = config_path.parent() {
            add(parent.to_path_buf(), true, true);
        }
        add(config_path.to_path_buf(), false, false);
        if !config.kernel.as_os_str().is_empty() {
            add(config.kernel.clone(), false, false);
        }
        if let Some(initrd) = &config.initrd {
            add(initrd.clone(), false, false);
        }
        if let Some(fw) = &config.firmware {
            add(fw.clone(), false, false);
        }
        if let Ok(Some(nvram)) = config.nvram_path() {
            add(nvram, false, true);
        }
        if let Ok(Some(state)) = config.tpm_state_path() {
            add(state, false, true);
        }
        add(config.serial_log.clone(), false, true);
        for disk in &config.disks {
            add(disk.path.clone(), false, !disk.read_only);
        }
        if let Some(iso) = &config.iso {
            for path in [&iso.installer, &iso.tools, &iso.drivers, &iso.unattend]
                .into_iter()
                .flatten()
            {
                add(path.clone(), false, false);
            }
        }
        for share in &config.shares {
            add(share.host_path.clone(), true, !share.read_only);
        }

        if let Ok(logs) = LogConfig::default_log_dir() {
            add(logs, true, true);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        add(
            PathBuf::from(&home).join("Library/Application Support/Ternvale"),
            true,
            true,
        );
        if let Ok(tmp) = std::env::temp_dir().canonicalize() {
            add(tmp, true, true);
        } else {
            add(std::env::temp_dir(), true, true);
        }
        if let Ok(exe) = std::env::current_exe() {
            add(exe.clone(), false, false);
            if let Some(parent) = exe.parent() {
                add(parent.to_path_buf(), true, false);
            }
        }
        for path in host_graphics_roots(config.gpu3d()) {
            add(path, true, false);
        }

        let helper_sock = vmnet_socket();
        if let Some(parent) = helper_sock.parent() {
            add(parent.to_path_buf(), true, true);
        }
        add(
            PathBuf::from(SYSTEM_SOCK)
                .parent()
                .unwrap_or(Path::new("/var/run"))
                .to_path_buf(),
            true,
            false,
        );

        // User-mode NAT and Virtualization.framework NAT (VZ always attaches
        // VZNATNetworkDeviceAttachment) need IP sockets. TODO(verify): VZ NAT
        // under Seatbelt on macOS 15+ (`network-outbound` vs extra mach names).
        let network_client = config.nics.iter().any(|nic| nic.backend == "user")
            || config.backend == ternvale_config::BackendKind::Vz
            || config.profile == ternvale_config::Profile::Macos;
        let paths: Vec<Grant> = set.into_iter().collect();
        let microphone = config.audio_input_enabled();
        let camera = config.camera_enabled();
        tracing::info!(
            target: "ternvale::sandbox",
            grants = paths.len(),
            network_client,
            microphone,
            camera,
            helper = %helper_sock.display(),
            "sandbox grants from vm config"
        );
        Self {
            paths,
            network_client,
            helper_sock,
            microphone,
            camera,
        }
    }
}

/// Homebrew / MoltenVK / locally built virglrenderer trees (`dlopen` + Vulkan ICD).
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all, fields(gpu3d))]
fn host_graphics_roots(gpu3d: bool) -> Vec<PathBuf> {
    if !gpu3d {
        return Vec::new();
    }
    let mut roots = Vec::new();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    for path in [
        PathBuf::from("/opt/homebrew"),
        PathBuf::from("/usr/local"),
        PathBuf::from(&home).join("Library/Application Support/Ternvale/host-gfx"),
        PathBuf::from(&home).join("Library/Application Support/vulkan"),
    ] {
        if path.exists() {
            roots.push(path);
        }
    }
    tracing::debug!(
        target: "ternvale::sandbox",
        n = roots.len(),
        "gpu3d host graphics path grants"
    );
    roots
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::Grants;
    use ternvale_config::VmConfig;

    #[test]
    fn user_nic_requests_network_client() {
        let dir = std::env::temp_dir().join(format!("tv-sb-grants-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cfg = dir.join("vm.toml");
        std::fs::write(&cfg, "ok").unwrap();
        let disk = dir.join("disk.img");
        std::fs::write(&disk, [0u8; 8]).unwrap();
        let share = dir.join("share");
        std::fs::create_dir_all(&share).unwrap();
        let iso = dir.join("os.iso");
        std::fs::write(&iso, [0u8; 8]).unwrap();
        let text = format!(
            "name = \"g\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"{}/s.log\"\n[[disks]]\npath = \"{}\"\nread_only = false\n[[nics]]\nbackend = \"user\"\n[[shares]]\ntag = \"host\"\nhost_path = \"{}\"\nmount_point = \"/mnt/t\"\n[iso]\ninstaller = \"{}\"\n",
            dir.display(),
            disk.display(),
            share.display(),
            iso.display()
        );
        let config = VmConfig::parse(&text).unwrap();
        let grants = Grants::from_config(&config, &cfg);
        assert!(grants.network_client);
        assert!(grants.paths.iter().any(|g| g.path == disk));
        assert!(grants.paths.iter().any(|g| g.path == iso && !g.write));
        assert!(grants.paths.iter().any(|g| g.path == share && g.directory));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vmnet_only_has_no_network_client() {
        let text = "name = \"g\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n[[nics]]\nbackend = \"vmnet\"\n";
        let config = VmConfig::parse(text).unwrap();
        let grants = Grants::from_config(&config, Path::new("/tmp/vm.toml"));
        assert!(!grants.network_client);
    }

    #[test]
    fn gpu3d_grants_homebrew_when_enabled() {
        let text = "name = \"g\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n[gpu]\ngpu3d = true\n";
        let config = VmConfig::parse(text).unwrap();
        let grants = Grants::from_config(&config, Path::new("/tmp/vm.toml"));
        let homebrew = Path::new("/opt/homebrew");
        if homebrew.exists() {
            assert!(
                grants
                    .paths
                    .iter()
                    .any(|g| g.path == homebrew && g.directory && !g.write),
                "gpu3d must allow reading Homebrew dylibs"
            );
        }
    }

    #[test]
    fn vz_requests_network_client() {
        let text = "name = \"g\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\nbackend = \"vz\"\n[tpm]\nenabled = false\n";
        let config = VmConfig::parse(text).unwrap();
        let grants = Grants::from_config(&config, Path::new("/tmp/vm.toml"));
        assert!(grants.network_client);
    }

    #[test]
    fn microphone_and_camera_grants_follow_opt_in() {
        let off =
            "name = \"g\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n";
        let grants = Grants::from_config(&VmConfig::parse(off).unwrap(), Path::new("/tmp/vm.toml"));
        assert!(!grants.microphone);
        assert!(!grants.camera);
        let on = format!("{off}[audio]\ninput = true\n[camera]\nenabled = true\n");
        let grants = Grants::from_config(&VmConfig::parse(&on).unwrap(), Path::new("/tmp/vm.toml"));
        assert!(grants.microphone);
        assert!(grants.camera);
    }
}
