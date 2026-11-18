//! Guest install of distro `qemu-user-static` (GPL, not bundled).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{bail, Context, Result};

use crate::client;
use crate::cp_cmd;
use crate::exec_cmd;
use crate::guest_setup;
use crate::json_out;
use crate::paths;
use crate::protocol::{Request, Response};

const LICENSE_NOTE: &str = "\
qemu-user-static is GPL. Ternvale does not bundle QEMU and does not link it.
The guest will install its distro package (qemu-user-static / qemu-x86_64) so
QEMU runs as a separate binfmt interpreter. Type yes to continue, or anything
else to cancel:";

const GUEST_DIR: &str = "/usr/local/lib/ternvale/qemu-user";
const GUEST_INSTALL: &str = "/usr/local/lib/ternvale/qemu-user/install-qemu-user.sh";
const GUEST_METRICS: &str = "/var/lib/ternvale/x86-metrics.json";

/// `ternvale x86 install-qemu-user`.
#[tracing::instrument(
    level = "info",
    target = "ternvale::compat",
    skip_all,
    fields(vm = ?vm, root = ?root, dry_run, yes)
)]
pub fn install_qemu_user(
    vm: Option<&str>,
    root: Option<&Path>,
    dry_run: bool,
    yes: bool,
    json: bool,
) -> Result<ExitCode> {
    if !read_consent(yes)? {
        tracing::warn!(target: "ternvale::compat", "qemu-user install cancelled");
        bail!("qemu-user install cancelled (type yes; QEMU is GPL and is not bundled)");
    }
    if let Some(root) = root {
        return install_on_root(root, dry_run, json);
    }
    let Some(vm) = vm else {
        bail!("pass a VM name or --root DIR");
    };
    install_in_guest(vm, dry_run, json)
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
fn read_consent(yes: bool) -> Result<bool> {
    if yes {
        tracing::debug!(target: "ternvale::compat", "qemu-user consent from --yes");
        return Ok(true);
    }
    if std::env::var_os("TERNVALE_QEMU_USER_CONSENT").as_deref()
        == Some(std::ffi::OsStr::new("yes"))
    {
        tracing::debug!(target: "ternvale::compat", "qemu-user consent from TERNVALE_QEMU_USER_CONSENT");
        return Ok(true);
    }
    println!("{LICENSE_NOTE}");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("read qemu-user GPL consent")?;
    Ok(consented(&line))
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
pub fn consented(line: &str) -> bool {
    line.trim() == "yes"
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all, fields(root = %root.display(), dry_run))]
fn install_on_root(root: &Path, dry_run: bool, json: bool) -> Result<ExitCode> {
    let out = run_script(root, dry_run)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("install-qemu-user.sh failed: {err}");
    }
    json_out::emit(
        json,
        serde_json::json!({
            "ok": true,
            "root": root.display().to_string(),
            "dry_run": dry_run,
            "bundled": false,
            "license": "GPL",
        }),
        || {
            println!(
                "qemu-user recipe written under {} (GPL, not bundled)",
                root.display()
            );
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all, fields(root = %root.display()))]
pub fn run_script(root: &Path, dry_run: bool) -> Result<std::process::Output> {
    let script = host_install_script()?;
    let mut cmd = Command::new("sh");
    cmd.env_remove("TERNVALE_X86_BREAK_GUEST_NET");
    cmd.arg(&script).arg("--root").arg(root);
    if dry_run {
        cmd.arg("--dry-run");
    }
    tracing::info!(
        target: "ternvale::compat",
        script = %script.display(),
        root = %root.display(),
        dry_run,
        "running install-qemu-user.sh"
    );
    cmd.output()
        .with_context(|| format!("run {}", script.display()))
}

#[cfg(test)]
#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
fn run_script_break_guest_net(root: &Path) -> Result<std::process::Output> {
    let script = host_install_script()?;
    Command::new("sh")
        .env("TERNVALE_X86_BREAK_GUEST_NET", "1")
        .arg(&script)
        .arg("--root")
        .arg(root)
        .output()
        .with_context(|| format!("run {}", script.display()))
}

#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
fn host_install_script() -> Result<PathBuf> {
    let tools = guest_setup::guest_tools_dir()?;
    let script = tools.join("qemu-user/install-qemu-user.sh");
    if !script.is_file() {
        bail!("missing {}", script.display());
    }
    Ok(script)
}

#[tracing::instrument(
    level = "info",
    target = "ternvale::compat",
    skip_all,
    fields(vm, dry_run)
)]
fn install_in_guest(vm: &str, dry_run: bool, json: bool) -> Result<ExitCode> {
    let tools = guest_setup::guest_tools_dir()?;
    let dir = tools.join("qemu-user");
    let mkdir = vec!["mkdir".into(), "-p".into(), GUEST_DIR.into()];
    exec_cmd::exec(vm, Some(30), &mkdir, false).context("mkdir qemu-user dest")?;
    for name in ["install-qemu-user.sh", "metrics.sh"] {
        let src = dir.join(name);
        if !src.is_file() {
            bail!("missing {}", src.display());
        }
        let dest = format!("{vm}:{GUEST_DIR}/{name}");
        tracing::debug!(
            target: "ternvale::compat",
            vm,
            src = %src.display(),
            dest = %dest,
            "copy qemu-user helper"
        );
        cp_cmd::cp(&src.display().to_string(), &dest, false)
            .with_context(|| format!("copy {}", src.display()))?;
    }
    let chmod = vec!["chmod".into(), "0755".into(), GUEST_INSTALL.into()];
    exec_cmd::exec(vm, Some(30), &chmod, false).context("chmod install-qemu-user.sh")?;
    let mut argv = vec!["sh".into(), GUEST_INSTALL.into()];
    if dry_run {
        argv.push("--dry-run".into());
    }
    tracing::info!(
        target: "ternvale::compat",
        vm,
        dry_run,
        "running guest qemu-user installer"
    );
    let code =
        exec_cmd::exec(vm, Some(45), &argv, false).context("run guest qemu-user installer")?;
    json_out::emit(
        json,
        serde_json::json!({
            "ok": true,
            "vm": vm,
            "dry_run": dry_run,
            "bundled": false,
            "license": "GPL",
        }),
        || {
            println!("qemu-user-static requested in {vm} (GPL, distro package, separate program)");
        },
    )?;
    Ok(code)
}

/// Read guest `/var/lib/ternvale/x86-metrics.json` and add it to `query-stats`.
#[tracing::instrument(level = "info", target = "ternvale::compat", skip_all, fields(vm = name))]
pub fn scrape_metrics(name: &str, json: bool) -> Result<ExitCode> {
    let host = std::env::temp_dir().join(format!(
        "ternvale-x86-metrics-{}-{}",
        std::process::id(),
        name
    ));
    cp_cmd::cp(
        &format!("{name}:{GUEST_METRICS}"),
        &host.display().to_string(),
        false,
    )
    .context("copy guest x86-metrics.json")?;
    let body = fs::read_to_string(&host).context("read scraped x86-metrics.json")?;
    let _ = fs::remove_file(&host);
    let value: serde_json::Value =
        serde_json::from_str(&body).context("parse guest x86-metrics.json")?;
    let engine = value
        .get("engine")
        .and_then(|v| v.as_str())
        .unwrap_or("qemu-user")
        .to_string();
    let processes = value.get("processes").and_then(|v| v.as_u64()).unwrap_or(0);
    let failures = value.get("failures").and_then(|v| v.as_u64()).unwrap_or(0);
    let socket = paths::socket_path(name)?;
    let stream =
        client::connect(&socket)?.map_err(|_| anyhow::anyhow!("vm {name} is not running"))?;
    let response: Response = client::exchange(
        stream,
        &Request::X86Metrics {
            engine: engine.clone(),
            processes,
            failures,
        },
    )
    .context("x86-metrics control command")?;
    if !response.ok {
        bail!(
            "x86-metrics: {}",
            response.error.unwrap_or_else(|| "unknown error".into())
        );
    }
    tracing::info!(
        target: "ternvale::compat",
        vm = name,
        engine = engine.as_str(),
        processes,
        failures,
        "scraped guest x86 metrics"
    );
    json_out::emit(
        json,
        serde_json::json!({
            "ok": true,
            "vm": name,
            "engine": engine,
            "processes": processes,
            "failures": failures,
        }),
        || {
            println!("{name}: x86 {engine} processes={processes} failures={failures}");
        },
    )?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_is_exactly_yes() {
        assert!(consented("yes\n"));
        assert!(!consented("y\n"));
        assert!(!consented("YES\n"));
    }

    #[test]
    fn root_debian_writes_gpl_doc_not_qemu_binary() {
        let td = std::env::temp_dir().join(format!(
            "ternvale-qemu-user-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(td.join("etc")).unwrap();
        fs::write(td.join("etc/os-release"), "ID=debian\nID_LIKE=debian\n").unwrap();
        let out = run_script(&td, false).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let doc = fs::read_to_string(td.join("usr/share/doc/ternvale/QEMU-USER.txt")).unwrap();
        assert!(doc.contains("GPL"), "{doc}");
        assert!(doc.contains("qemu-user-static"), "{doc}");
        assert!(td.join("var/lib/ternvale/x86-metrics.json").is_file());
        assert!(!td.join("usr/bin/qemu-x86_64-static").exists());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("skipped=root"), "{err}");
        let _ = fs::remove_dir_all(td);
    }

    #[test]
    fn guest_network_break_fails_without_hanging() {
        let td = std::env::temp_dir().join(format!(
            "ternvale-qemu-netbreak-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(td.join("etc")).unwrap();
        fs::write(td.join("etc/os-release"), "ID=debian\nID_LIKE=debian\n").unwrap();
        let out = run_script_break_guest_net(&td).unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{err}");
        assert!(err.contains("guest-network-unreachable"), "{err}");
        let _ = fs::remove_dir_all(td);
    }
}
