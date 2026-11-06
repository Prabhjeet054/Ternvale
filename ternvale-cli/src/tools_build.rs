//! Stage tools files and write `ternvale-tools.iso`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::tools_hybrid::make_hybrid_iso;
use crate::tools_manifest::{ManifestFile, ToolsManifest};

const INSTALL_PS1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../scripts/windows/Install-TernvaleTools.ps1"
));

const FETCH_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../scripts/fetch-virtio-win.sh"
);

const README: &str = "Ternvale guest tools\n\n\
Run Install-TernvaleTools.ps1 (or install.ps1) as Administrator.\n\
That installs ARM64 virtio-win drivers with pnputil and the\n\
TernvaleGuestAgent service (ternvale-agent.exe --install).\n\
virtio-win is not part of the Ternvale source tree; see docs/LICENSES.md.\n";

/// Inputs for [`build_iso`].
pub struct BuildIsoArgs {
    /// User virtio-win ISO or extract tree.
    pub virtio_win: Option<PathBuf>,
    /// Fetch Fedora virtio-win when `virtio_win` is unset.
    pub fetch: bool,
    /// `ternvale-agent.exe`.
    pub agent: Option<PathBuf>,
    /// Output ISO.
    pub out: PathBuf,
    /// Override `agent_version` in the manifest (tests / rebuilt ISO).
    pub agent_version: Option<String>,
}

/// Paths written by a successful build.
pub struct BuiltIso {
    /// Hybrid ISO.
    pub iso: PathBuf,
    /// Sidecar JSON next to the ISO.
    pub sidecar: PathBuf,
    /// Parsed manifest (same bytes as on the ISO).
    pub manifest: ToolsManifest,
}

/// Assemble the ISO and a sidecar manifest.
#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all, fields(out = %args.out.display(), fetch = args.fetch))]
pub fn build_iso(args: BuildIsoArgs) -> Result<BuiltIso> {
    let agent = resolve_agent(args.agent.as_deref())?;
    let virtio = resolve_virtio_win(args.virtio_win.as_deref(), args.fetch)?;
    let virtio_label = virtio_win_label(&virtio);
    tracing::info!(
        target: "ternvale::cli",
        agent = %agent.display(),
        virtio_win = %virtio.display(),
        virtio_label = %virtio_label,
        "staging ternvale-tools.iso"
    );

    let staging = unique_dir("ternvale-tools-stage")?;
    let result = (|| {
        fs::create_dir_all(staging.join("agent")).context("create agent staging dir")?;
        fs::write(staging.join("install.ps1"), INSTALL_PS1).context("write install.ps1")?;
        fs::write(staging.join("Install-TernvaleTools.ps1"), INSTALL_PS1)
            .context("write Install-TernvaleTools.ps1")?;
        fs::write(staging.join("README.txt"), README).context("write README.txt")?;
        fs::copy(&agent, staging.join("agent/ternvale-agent.exe"))
            .with_context(|| format!("copy agent {}", agent.display()))?;
        stage_virtio_win(&virtio, &staging.join("virtio-win"))?;

        let files = hash_tree(&staging)?;
        let version = env!("CARGO_PKG_VERSION").to_string();
        let agent_version = args
            .agent_version
            .clone()
            .unwrap_or_else(|| version.clone());
        let manifest = ToolsManifest::new(version, agent_version, virtio_label, files);
        let json = serde_json::to_vec_pretty(&manifest).context("serialize MANIFEST.json")?;
        fs::write(staging.join("MANIFEST.json"), &json).context("write MANIFEST.json")?;

        if args.out.exists() {
            fs::remove_file(&args.out)
                .with_context(|| format!("remove existing {}", args.out.display()))?;
        }
        make_hybrid_iso(&staging, &args.out)?;
        let sidecar = sidecar_path(&args.out);
        fs::write(&sidecar, &json)
            .with_context(|| format!("write sidecar {}", sidecar.display()))?;
        tracing::info!(
            target: "ternvale::cli",
            iso = %args.out.display(),
            sidecar = %sidecar.display(),
            files = manifest.files.len(),
            "ternvale-tools.iso ready"
        );
        Ok(BuiltIso {
            iso: args.out.clone(),
            sidecar,
            manifest,
        })
    })();
    if let Err(error) = fs::remove_dir_all(&staging) {
        tracing::warn!(
            target: "ternvale::cli",
            staging = %staging.display(),
            error = %error,
            "could not remove tools staging directory"
        );
    }
    result
}

/// Sidecar JSON next to `iso` (`ternvale-tools.iso.manifest.json`).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn sidecar_path(iso: &Path) -> PathBuf {
    let mut name = iso
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "ternvale-tools.iso".into());
    name.push(".manifest.json");
    match iso.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn resolve_agent(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return require_file("agent", path);
    }
    if let Some(path) = std::env::var_os("TERNVALE_WINDOWS_AGENT") {
        return require_file("TERNVALE_WINDOWS_AGENT", Path::new(&path));
    }
    let fallback =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/guest/ternvale-agent.exe");
    if fallback.is_file() {
        return Ok(fallback);
    }
    Err(anyhow!(
        "no Windows agent PE; pass --agent PATH or run scripts/build-windows-agent.sh (looked at {})",
        fallback.display()
    ))
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn resolve_virtio_win(explicit: Option<&Path>, fetch: bool) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return require_existing("virtio-win", path);
    }
    if let Some(path) = std::env::var_os("TERNVALE_VIRTIO_WIN") {
        return require_existing("TERNVALE_VIRTIO_WIN", Path::new(&path));
    }
    let repo_iso =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-assets/virtio-win/virtio-win.iso");
    if repo_iso.is_file() {
        return Ok(repo_iso);
    }
    if fetch {
        return fetch_virtio_win();
    }
    Err(anyhow!(
        "no virtio-win ISO or extract tree; pass --virtio-win PATH or --fetch (license: docs/LICENSES.md)"
    ))
}

#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all)]
fn fetch_virtio_win() -> Result<PathBuf> {
    let dest = cache_dir()?.join("virtio-win");
    fs::create_dir_all(&dest).with_context(|| format!("create {}", dest.display()))?;
    let script = Path::new(FETCH_SCRIPT);
    if !script.is_file() {
        bail!("fetch script missing: {}", script.display());
    }
    tracing::info!(
        target: "ternvale::cli",
        dest = %dest.display(),
        "fetching virtio-win (see docs/LICENSES.md)"
    );
    let status = Command::new("bash")
        .arg(script)
        .arg(&dest)
        .status()
        .context("spawn scripts/fetch-virtio-win.sh")?;
    if !status.success() {
        bail!("scripts/fetch-virtio-win.sh failed ({status})");
    }
    let iso = dest.join("virtio-win.iso");
    require_file("fetched virtio-win.iso", &iso)
}

fn cache_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Caches/Ternvale"))
}

fn virtio_win_label(path: &Path) -> String {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    name.strip_prefix("virtio-win-").unwrap_or(name).to_string()
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(src = %src.display()))]
fn stage_virtio_win(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).context("create virtio-win staging dir")?;
    if src.is_dir() {
        copy_tree(src, dest)?;
        return Ok(());
    }
    tracing::info!(
        target: "ternvale::cli",
        src = %src.display(),
        dest = %dest.display(),
        "extracting virtio-win ISO"
    );
    let output = Command::new("tar")
        .args(["-xf"])
        .arg(src)
        .arg("-C")
        .arg(dest)
        .output()
        .context("spawn tar to extract virtio-win ISO")?;
    if !output.status.success() {
        bail!(
            "tar extract of {} failed ({}): {}",
            src.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn copy_tree(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", src.display()))?;
        let name = entry.file_name();
        let from = entry.path();
        let to = dest.join(&name);
        let meta = entry
            .metadata()
            .with_context(|| format!("stat {}", from.display()))?;
        if meta.file_type().is_symlink() {
            tracing::warn!(
                target: "ternvale::cli",
                path = %from.display(),
                "skipping symlink in virtio-win tree"
            );
            continue;
        }
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn hash_tree(root: &Path) -> Result<Vec<ManifestFile>> {
    let mut files = Vec::new();
    collect_hashes(root, root, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn collect_hashes(root: &Path, dir: &Path, files: &mut Vec<ManifestFile>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("read {}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let meta = entry
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?;
        if meta.is_dir() {
            collect_hashes(root, &path, files)?;
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = fs::read(&path).with_context(|| format!("hash {}", path.display()))?;
        let digest = Sha256::digest(&bytes);
        let sha256 = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        files.push(ManifestFile {
            path: rel,
            sha256,
            bytes: bytes.len() as u64,
        });
    }
    Ok(())
}

fn require_file(field: &str, path: &Path) -> Result<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    Err(anyhow!("{field} is not a file: {}", path.display()))
}

fn require_existing(field: &str, path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.to_path_buf());
    }
    Err(anyhow!("{field} does not exist: {}", path.display()))
}

fn unique_dir(prefix: &str) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// Listed by `bsdtar -tf` / `tar -tf` after a successful hybrid build.
pub fn iso_list(iso: &Path) -> Result<String> {
    let output = Command::new("tar")
        .args(["-tf"])
        .arg(iso)
        .output()
        .context("spawn tar -tf")?;
    if !output.status.success() {
        bail!(
            "tar -tf {} failed: {}",
            iso.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Write a dummy agent + virtio tree then build (unit tests).
#[cfg(test)]
pub fn build_test_iso(out: &Path, staging_src: &Path) -> Result<BuiltIso> {
    let agent = staging_src.join("ternvale-agent.exe");
    let virtio = staging_src.join("virtio-src");
    fs::create_dir_all(virtio.join("viostor/w11/ARM64")).context("virtio tree")?;
    fs::write(
        virtio.join("viostor/w11/ARM64/viostor.inf"),
        b"; dummy INF\n",
    )?;
    fs::write(&agent, b"MZ dummy agent")?;
    build_iso(BuildIsoArgs {
        virtio_win: Some(virtio),
        fetch: false,
        agent: Some(agent),
        out: out.to_path_buf(),
        agent_version: None,
    })
}
