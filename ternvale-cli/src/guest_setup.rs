//! Copy and run `guest-tools/ternvale-guest-setup` in a Linux guest.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{bail, Context, Result};

use crate::cp_cmd;
use crate::exec_cmd;
use crate::json_out;

const GUEST_SBIN: &str = "/usr/local/sbin/ternvale-guest-setup";
const GUEST_LIB: &str = "/usr/local/lib/ternvale/guest-setup";

/// Host directory that contains `ternvale-guest-setup` and `lib/guest-setup/`.
#[tracing::instrument(level = "debug", target = "ternvale::rosetta", skip_all)]
pub fn guest_tools_dir() -> Result<PathBuf> {
    if let Ok(override_dir) = std::env::var("TERNVALE_GUEST_TOOLS") {
        let path = PathBuf::from(override_dir);
        tracing::debug!(
            target: "ternvale::rosetta",
            path = %path.display(),
            "guest tools from TERNVALE_GUEST_TOOLS"
        );
        return Ok(path);
    }
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = crate_dir
        .parent()
        .map(|p| p.join("guest-tools"))
        .context("resolve workspace guest-tools")?;
    if workspace.join("ternvale-guest-setup").is_file() {
        return Ok(workspace);
    }
    let exe = std::env::current_exe().context("current_exe for guest-tools")?;
    if let Some(dir) = exe.parent() {
        for cand in [
            dir.join("guest-tools"),
            dir.join("../guest-tools"),
            dir.join("../share/ternvale/guest-tools"),
        ] {
            if cand.join("ternvale-guest-setup").is_file() {
                return Ok(cand);
            }
        }
    }
    bail!("guest-tools/ternvale-guest-setup not found (set TERNVALE_GUEST_TOOLS)")
}

/// Install the helper via the agent and run it (`--dry-run` / `--undo` forwarded).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::rosetta",
    skip_all,
    fields(vm = name, dry_run, undo)
)]
pub fn guest_setup(name: &str, dry_run: bool, undo: bool, json: bool) -> Result<ExitCode> {
    let tools = guest_tools_dir()?;
    tracing::info!(
        target: "ternvale::rosetta",
        vm = name,
        tools = %tools.display(),
        dry_run,
        undo,
        "installing ternvale-guest-setup"
    );
    let mkdir = vec![
        "mkdir".into(),
        "-p".into(),
        "/usr/local/sbin".into(),
        GUEST_LIB.into(),
    ];
    exec_cmd::exec(name, Some(30), &mkdir, false).context("mkdir guest-setup dest")?;
    let script = tools.join("ternvale-guest-setup");
    cp_cmd::cp(
        &script.display().to_string(),
        &format!("{name}:{GUEST_SBIN}"),
        false,
    )
    .context("copy ternvale-guest-setup")?;
    let chmod = vec!["chmod".into(), "0755".into(), GUEST_SBIN.into()];
    exec_cmd::exec(name, Some(30), &chmod, false).context("chmod guest-setup")?;
    let lib = tools.join("lib/guest-setup");
    for entry in fs::read_dir(&lib).with_context(|| format!("read {}", lib.display()))? {
        let entry = entry.context("guest-setup lib entry")?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sh") {
            continue;
        }
        let dest = format!(
            "{name}:{GUEST_LIB}/{}",
            path.file_name()
                .and_then(|n| n.to_str())
                .context("lib file name")?
        );
        tracing::debug!(
            target: "ternvale::rosetta",
            vm = name,
            src = %path.display(),
            dest = %dest,
            "copy guest-setup lib"
        );
        cp_cmd::cp(&path.display().to_string(), &dest, false)
            .with_context(|| format!("copy {}", path.display()))?;
    }
    let mut argv = vec!["sh".into(), GUEST_SBIN.into()];
    if dry_run {
        argv.push("--dry-run".into());
    }
    if undo {
        argv.push("--undo".into());
    }
    tracing::info!(
        target: "ternvale::rosetta",
        vm = name,
        argc = argv.len(),
        "running ternvale-guest-setup"
    );
    let code = exec_cmd::exec(name, Some(120), &argv, false).context("run ternvale-guest-setup")?;
    json_out::emit(
        json,
        serde_json::json!({
            "ok": true,
            "vm": name,
            "dry_run": dry_run,
            "undo": undo,
        }),
        || {
            println!(
                "guest-setup {name} dry_run={dry_run} undo={undo} (see guest stderr for actions)"
            );
        },
    )?;
    Ok(code)
}

/// `guest-tools/ternvale-guest-setup` plus `lib/guest-setup` (host tests / --root).
#[tracing::instrument(level = "debug", target = "ternvale::rosetta", skip_all)]
pub fn host_script() -> Result<PathBuf> {
    let dir = guest_tools_dir()?;
    let script = dir.join("ternvale-guest-setup");
    if !script.is_file() {
        bail!("missing {}", script.display());
    }
    Ok(script)
}

/// Run the POSIX helper against a fake root (no VM).
#[tracing::instrument(level = "debug", target = "ternvale::rosetta", skip_all)]
pub fn run_on_root(root: &Path, extra: &[&str]) -> Result<std::process::Output> {
    let script = host_script()?;
    let mut cmd = Command::new("sh");
    cmd.arg(&script).arg("--root").arg(root);
    cmd.args(extra);
    tracing::debug!(
        target: "ternvale::rosetta",
        root = %root.display(),
        extra = extra.len(),
        "guest-setup --root"
    );
    cmd.output()
        .with_context(|| format!("run {} --root", script.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Scratch {
        path: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "ternvale-guest-setup-{tag}-{}-{n}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_os_release(root: &Path, body: &str) {
        let etc = root.join("etc");
        fs::create_dir_all(&etc).unwrap();
        fs::write(etc.join("os-release"), body).unwrap();
    }

    fn stderr_str(out: &std::process::Output) -> String {
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    #[test]
    fn finds_workspace_guest_tools() {
        let dir = guest_tools_dir().unwrap();
        assert!(dir.join("ternvale-guest-setup").is_file());
        assert!(dir.join("lib/guest-setup/common.sh").is_file());
    }

    #[test]
    fn unsupported_distro_exits_2() {
        let td = Scratch::new("arch");
        write_os_release(
            &td.path,
            "ID=arch\nID_LIKE=archlinux\nPRETTY_NAME=\"Arch Linux\"\n",
        );
        let out = run_on_root(&td.path, &[]).unwrap();
        assert_eq!(out.status.code(), Some(2), "{}", stderr_str(&out));
        assert!(
            stderr_str(&out).contains("unsupported-distro"),
            "{}",
            stderr_str(&out)
        );
        assert!(!td.path.join("etc/fstab").exists());
    }

    #[test]
    fn dry_run_writes_nothing() {
        let td = Scratch::new("dry");
        write_os_release(
            &td.path,
            "ID=debian\nID_LIKE=debian\nVERSION_CODENAME=bookworm\n",
        );
        fs::create_dir_all(td.path.join("usr/lib/systemd/system")).unwrap();
        let out = run_on_root(&td.path, &["--dry-run"]).unwrap();
        assert!(out.status.success(), "{}", stderr_str(&out));
        let err = stderr_str(&out);
        assert!(err.contains("dry_run=true"), "{err}");
        assert!(!td
            .path
            .join("etc/systemd/system/mnt-rosetta.mount")
            .exists());
        assert!(!td
            .path
            .join("var/lib/ternvale/guest-setup-undo.sh")
            .exists());
    }

    #[test]
    fn debian_is_idempotent_and_undoes() {
        let td = Scratch::new("deb");
        write_os_release(
            &td.path,
            "ID=debian\nID_LIKE=debian\nVERSION_CODENAME=bookworm\n",
        );
        fs::create_dir_all(td.path.join("usr/lib/systemd/system")).unwrap();
        fs::create_dir_all(td.path.join("etc/docker")).unwrap();
        let first = run_on_root(&td.path, &[]).unwrap();
        assert!(first.status.success(), "{}", stderr_str(&first));
        let unit = td.path.join("etc/systemd/system/mnt-rosetta.mount");
        assert!(unit.is_file());
        let binfmt =
            fs::read_to_string(td.path.join("etc/binfmt.d/ternvale-rosetta.conf")).unwrap();
        assert!(binfmt.contains("\\x7fELF"), "{binfmt}");
        assert!(binfmt.contains("PCF"), "{binfmt}");
        let arch = fs::read_to_string(td.path.join("var/lib/dpkg/arch")).unwrap();
        assert!(arch.contains("amd64"), "{arch}");
        assert!(td.path.join("etc/docker").exists());
        assert!(td
            .path
            .join("etc/systemd/system/docker.service.d/ternvale-rosetta.conf")
            .is_file());
        let drop = fs::read_to_string(td.path.join("etc/profile.d/ternvale-rosetta.sh")).unwrap();
        assert!(drop.contains("linux/amd64"), "{drop}");

        let second = run_on_root(&td.path, &[]).unwrap();
        assert!(second.status.success(), "{}", stderr_str(&second));
        let err2 = stderr_str(&second);
        assert!(err2.contains("action=unchanged"), "{err2}");
        assert!(
            err2.contains("changed=0") || !err2.contains("action=changed"),
            "{err2}"
        );

        let undo = run_on_root(&td.path, &["--undo"]).unwrap();
        assert!(undo.status.success(), "{}", stderr_str(&undo));
        assert!(!unit.is_file(), "undo should remove mount unit");
    }

    #[test]
    fn ubuntu_adds_amd64_apt_and_warns() {
        let td = Scratch::new("ubu");
        write_os_release(
            &td.path,
            "ID=ubuntu\nID_LIKE=debian\nVERSION_CODENAME=jammy\n",
        );
        fs::create_dir_all(td.path.join("usr/lib/systemd/system")).unwrap();
        let out = run_on_root(&td.path, &[]).unwrap();
        assert!(out.status.success(), "{}", stderr_str(&out));
        let err = stderr_str(&out);
        assert!(err.contains("mixed arm64+amd64"), "{err}");
        let list =
            fs::read_to_string(td.path.join("etc/apt/sources.list.d/ternvale-amd64.list")).unwrap();
        assert!(list.contains("archive.ubuntu.com"), "{list}");
        assert!(list.contains("jammy"), "{list}");
        assert!(list.contains("[arch=amd64]"), "{list}");
    }

    #[test]
    fn fedora_and_alpine_document_multiarch() {
        let fedora = Scratch::new("fed");
        write_os_release(
            &fedora.path,
            "ID=fedora\nID_LIKE=fedora rhel\nVERSION_ID=40\n",
        );
        fs::create_dir_all(fedora.path.join("usr/lib/systemd/system")).unwrap();
        let out = run_on_root(&fedora.path, &[]).unwrap();
        assert!(out.status.success(), "{}", stderr_str(&out));
        assert!(fedora
            .path
            .join("usr/share/doc/ternvale/MULTIARCH-FEDORA.txt")
            .is_file());
        let second = run_on_root(&fedora.path, &[]).unwrap();
        assert!(second.status.success(), "{}", stderr_str(&second));
        let err2 = stderr_str(&second);
        assert!(
            err2.contains("changed=0"),
            "fedora second run must be a no-op: {err2}"
        );

        let alpine = Scratch::new("alp");
        write_os_release(&alpine.path, "ID=alpine\nVERSION_ID=3.20.3\n");
        let out = run_on_root(&alpine.path, &[]).unwrap();
        assert!(out.status.success(), "{}", stderr_str(&out));
        let fstab = fs::read_to_string(alpine.path.join("etc/fstab")).unwrap();
        assert!(fstab.contains("virtiofs"), "{fstab}");
        assert!(fstab.contains("rosetta"), "{fstab}");
        assert!(alpine
            .path
            .join("etc/local.d/ternvale-binfmt.start")
            .is_file());
        assert!(alpine
            .path
            .join("usr/share/doc/ternvale/MULTIARCH-ALPINE.txt")
            .is_file());
    }
}
