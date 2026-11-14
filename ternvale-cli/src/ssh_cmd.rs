//! `ternvale ssh <vm>`: port-forward + key, then spawn OpenSSH.

use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{bail, Context, Result};
use ternvale_config::{ForwardProto, VmConfig};

use crate::json_out;
use crate::paths;

const DEFAULT_HOST_PORT: u16 = 2222;
const GUEST_SSH: u16 = 22;

/// Ensure a host key exists, pick a forwarded port, optionally install the
/// pubkey via the agent, then exec `ssh`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name))]
pub fn ssh(name: &str, json: bool) -> Result<ExitCode> {
    let dir = paths::vm_dir(name)?;
    let config_path = dir.join("config.toml");
    let config = if config_path.is_file() {
        VmConfig::from_path(&config_path)
            .with_context(|| format!("load {}", config_path.display()))?
    } else {
        bail!("no library VM {name} (missing {})", config_path.display());
    };
    let host_port = ssh_host_port(&config).unwrap_or(DEFAULT_HOST_PORT);
    let key_dir = dir.join("ssh");
    std::fs::create_dir_all(&key_dir).with_context(|| format!("create {}", key_dir.display()))?;
    let key = key_dir.join("id_ed25519");
    if !key.is_file() {
        generate_key(&key, name)?;
    }
    let pub_path = key_dir.join("id_ed25519.pub");
    if config.allow_exec() {
        if let Ok(pubkey) = std::fs::read_to_string(&pub_path) {
            let _ = crate::exec_cmd::exec(
                name,
                Some(15),
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    format!(
                        "mkdir -p ~/.ssh && umask 077 && touch ~/.ssh/authorized_keys && grep -qxF {q} ~/.ssh/authorized_keys || echo {q} >> ~/.ssh/authorized_keys",
                        q = shell_single(pubkey.trim())
                    ),
                ],
                json,
            );
        }
    } else {
        tracing::info!(
            target: "ternvale::cli",
            vm = name,
            "ssh key not installed in guest (agent.allow_exec is false); copy {}.pub yourself",
            key.display()
        );
    }
    if json {
        json_out::emit(
            true,
            serde_json::json!({
                "ok": true,
                "vm": name,
                "host_port": host_port,
                "identity": key.display().to_string(),
            }),
            || {},
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    tracing::info!(
        target: "ternvale::cli",
        vm = name,
        host_port,
        identity = %key.display(),
        "starting ssh"
    );
    let status = Command::new("ssh")
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-p",
            &host_port.to_string(),
            "-i",
        ])
        .arg(&key)
        .arg("localhost")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn ssh")?;
    let code = status.code().unwrap_or(1);
    let raw = if (0..=255).contains(&code) {
        code as u8
    } else {
        1
    };
    Ok(ExitCode::from(raw))
}

fn ssh_host_port(config: &VmConfig) -> Option<u16> {
    for nic in &config.nics {
        for fwd in &nic.forwards {
            if fwd.guest_port == GUEST_SSH && fwd.proto == ForwardProto::Tcp {
                return Some(fwd.host_port);
            }
        }
    }
    None
}

fn generate_key(path: &PathBuf, name: &str) -> Result<()> {
    let status = Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            &format!("ternvale@{name}"),
            "-f",
        ])
        .arg(path)
        .status()
        .context("spawn ssh-keygen")?;
    if !status.success() {
        bail!("ssh-keygen failed with {status}");
    }
    tracing::info!(target: "ternvale::cli", path = %path.display(), "created ssh identity");
    Ok(())
}

fn shell_single(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_config::{Nic, PortForward};

    #[test]
    fn finds_forwarded_guest_22() {
        let mut config = VmConfig {
            name: "x".into(),
            profile: Default::default(),
            cpus: 1,
            ram_mib: 256,
            ram_min_mib: None,
            kernel: "/k".into(),
            initrd: None,
            cmdline: String::new(),
            boot_disk: false,
            disks: Vec::new(),
            iso: None,
            nics: {
                let mut nic = Nic::user();
                nic.forwards.push(PortForward {
                    host_port: 2222,
                    guest_port: 22,
                    proto: ForwardProto::Tcp,
                });
                vec![nic]
            },
            serial_log: "/tmp/s".into(),
            firmware: None,
            nvram: None,
            firmware_tables: None,
            vsock: None,
            tpm: None,
            usb: None,
            audio: None,
            shares: Vec::new(),
            power: Default::default(),
            encryption: false,
            rosetta: false,
            agent: Default::default(),
            backend: Default::default(),
        };
        assert_eq!(ssh_host_port(&config), Some(2222));
        config.nics.clear();
        assert_eq!(ssh_host_port(&config), None);
    }

    #[test]
    fn quotes_single_quotes_in_keys() {
        assert_eq!(shell_single("a'b"), "'a'\\''b'");
    }
}
