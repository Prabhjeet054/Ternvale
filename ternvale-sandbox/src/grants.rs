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

        let network_client = config.nics.iter().any(|nic| nic.backend == "user");
        let paths: Vec<Grant> = set.into_iter().collect();
        tracing::info!(
            target: "ternvale::sandbox",
            grants = paths.len(),
            network_client,
            helper = %helper_sock.display(),
            "sandbox grants from vm config"
        );
        Self {
            paths,
            network_client,
            helper_sock,
        }
    }
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
}
