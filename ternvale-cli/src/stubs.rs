//! `~/Applications/Ternvale Apps/<vm>/<App>.app` launcher stubs.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use ternvale_agent_proto::GuestApp;

/// Marker magic inside each stub.
pub const STUB_MAGIC: &str = "TVST";
/// Marker version.
pub const STUB_VERSION: u32 = 1;
/// Override for tests (`TERNVALE_APPS_DIR`).
pub const APPS_DIR_ENV: &str = "TERNVALE_APPS_DIR";

/// JSON in `Contents/Resources/ternvale-stub.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StubMarker {
    pub magic: String,
    pub version: u32,
    pub vm: String,
    pub app_id: String,
}

/// Directory holding generated `.app` stubs.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn apps_root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(APPS_DIR_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Applications/Ternvale Apps"))
}

/// Per-VM stub directory.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm))]
pub fn vm_apps_dir(vm: &str) -> Result<PathBuf> {
    Ok(apps_root()?.join(sanitize(vm)))
}

/// Write one `.app` per listed guest app. Returns how many were written.
#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all, fields(vm, count = apps.len()))]
pub fn install(vm: &str, apps: &[GuestApp], ternvale_bin: &Path) -> Result<u32> {
    let root = vm_apps_dir(vm)?;
    std::fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
    let mut written = 0u32;
    for app in apps {
        write_stub(vm, app, ternvale_bin, &root)?;
        written += 1;
    }
    tracing::info!(
        target: "ternvale::cli",
        vm,
        written,
        dir = %root.display(),
        "installed launcher stubs"
    );
    Ok(written)
}

/// Remove stubs that carry our marker. `vm = None` removes every VM's stubs.
#[tracing::instrument(level = "info", target = "ternvale::cli", skip_all, fields(vm = vm.unwrap_or("*")))]
pub fn uninstall(vm: Option<&str>) -> Result<u32> {
    let root = apps_root()?;
    if !root.is_dir() {
        return Ok(0);
    }
    let mut removed = 0u32;
    let dirs: Vec<PathBuf> = if let Some(vm) = vm {
        vec![root.join(sanitize(vm))]
    } else {
        std::fs::read_dir(&root)
            .with_context(|| format!("read {}", root.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect()
    };
    for dir in dirs {
        removed += uninstall_dir(&dir)?;
        if dir.is_dir()
            && std::fs::read_dir(&dir)
                .ok()
                .and_then(|mut i| i.next())
                .is_none()
        {
            let _ = std::fs::remove_dir(&dir);
        }
    }
    if vm.is_none()
        && root.is_dir()
        && std::fs::read_dir(&root)
            .ok()
            .and_then(|mut i| i.next())
            .is_none()
    {
        let _ = std::fs::remove_dir(&root);
    }
    tracing::info!(target: "ternvale::cli", removed, "uninstalled launcher stubs");
    Ok(removed)
}

fn uninstall_dir(dir: &Path) -> Result<u32> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut removed = 0u32;
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("app") {
            continue;
        }
        if read_marker(&path)?.is_some() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("remove stub {}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn write_stub(vm: &str, app: &GuestApp, bin: &Path, root: &Path) -> Result<()> {
    let name = sanitize(&app.name);
    if name.is_empty() {
        bail!("app name is empty");
    }
    let bundle = root.join(format!("{name}.app"));
    let macos = bundle.join("Contents/MacOS");
    let resources = bundle.join("Contents/Resources");
    std::fs::create_dir_all(&macos).with_context(|| format!("create {}", macos.display()))?;
    std::fs::create_dir_all(&resources)
        .with_context(|| format!("create {}", resources.display()))?;
    let marker = StubMarker {
        magic: STUB_MAGIC.into(),
        version: STUB_VERSION,
        vm: vm.to_string(),
        app_id: app.id.clone(),
    };
    std::fs::write(
        resources.join("ternvale-stub.json"),
        serde_json::to_vec_pretty(&marker).context("marker json")?,
    )
    .context("write stub marker")?;
    let exe = macos.join("launcher");
    let script = format!(
        "#!/bin/sh\nexec \"{}\" apps launch --vm {} --id {} -- \"$@\"\n",
        bin.display(),
        shell_single(vm),
        shell_single(&app.id),
    );
    std::fs::write(&exe, script).context("write stub launcher")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))
            .context("chmod stub launcher")?;
    }
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        info_plist(&name, &app.extensions),
    )
    .context("write Info.plist")?;
    Ok(())
}

fn read_marker(bundle: &Path) -> Result<Option<StubMarker>> {
    let path = bundle.join("Contents/Resources/ternvale-stub.json");
    if !path.is_file() {
        return Ok(None);
    }
    let marker: StubMarker = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;
    if marker.magic != STUB_MAGIC {
        return Ok(None);
    }
    if marker.version != STUB_VERSION {
        bail!(
            "stub {} version {} is unsupported",
            path.display(),
            marker.version
        );
    }
    Ok(Some(marker))
}

fn info_plist(name: &str, extensions: &[String]) -> String {
    let mut docs = String::new();
    if !extensions.is_empty() {
        let exts: String = extensions
            .iter()
            .map(|ext| format!("        <string>{}</string>\n", xml_escape(ext)))
            .collect();
        docs = format!(
            r#"  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key>
      <string>Ternvale guest document</string>
      <key>CFBundleTypeRole</key>
      <string>Viewer</string>
      <key>LSHandlerRank</key>
      <string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array>
        <string>public.data</string>
      </array>
      <key>CFBundleTypeExtensions</key>
      <array>
{exts}      </array>
    </dict>
  </array>
"#
        );
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>{}</string>
  <key>CFBundleIdentifier</key>
  <string>dev.ternvale.stub.{}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleExecutable</key>
  <string>launcher</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
{docs}</dict>
</plist>
"#,
        xml_escape(name),
        xml_escape(name),
        docs = docs
    )
}

fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == ' ' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    cleaned.trim().to_string()
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn shell_single(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_and_uninstall_only_removes_marked_bundles() {
        let dir = std::env::temp_dir().join(format!("tv-stubs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var(APPS_DIR_ENV, &dir);
        let apps = vec![GuestApp {
            id: "Notepad.lnk".into(),
            name: "Notepad".into(),
            extensions: vec!["txt".into()],
            ..GuestApp::default()
        }];
        let n = install("lab", &apps, Path::new("/usr/bin/true")).expect("install");
        assert_eq!(n, 1);
        let bundle = dir.join("lab/Notepad.app");
        assert!(bundle.join("Contents/MacOS/launcher").is_file());
        let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).expect("plist");
        assert!(plist.contains("txt"), "{plist}");
        let foreign = dir.join("lab/Other.app");
        std::fs::create_dir_all(foreign.join("Contents")).expect("foreign");
        assert_eq!(uninstall(Some("lab")).expect("un"), 1);
        assert!(!bundle.exists());
        assert!(foreign.exists());
        std::env::remove_var(APPS_DIR_ENV);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
