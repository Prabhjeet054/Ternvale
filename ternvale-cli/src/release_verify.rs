//! Post-sign checks: nested `codesign --verify` and hypervisor entitlements.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Entitlements the VMM helper must still have after sign / staple.
pub const HYPERVISOR_KEY: &str = "com.apple.security.hypervisor";
/// USB / device passthrough companion bit in the same plist.
pub const DEVICE_ACCESS_KEY: &str = "com.apple.vm.device-access";

/// True when `key` is present and followed by `<true/>` (codesign XML dump).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(key))]
pub fn entitlement_enabled(plist: &str, key: &str) -> bool {
    let needle = format!("<key>{key}</key>");
    let Some(rest) = plist.split(&needle).nth(1) else {
        tracing::debug!(target: "ternvale::cli", key, "entitlement key missing");
        return false;
    };
    let rest = rest.trim_start();
    let on = rest.starts_with("<true/>") || rest.starts_with("<true></true>");
    tracing::debug!(target: "ternvale::cli", key, on, "entitlement value");
    on
}

/// Run `codesign --display --entitlements :-` and return stdout (XML).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn dump_entitlements(path: &Path) -> Result<String> {
    let output = Command::new("codesign")
        .args(["--display", "--entitlements", ":-"])
        .arg(path)
        .output()
        .context("run codesign --display --entitlements")?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        tracing::error!(
            target: "ternvale::cli",
            path = %path.display(),
            status = ?output.status.code(),
            stderr = %stderr.trim(),
            "codesign --display failed"
        );
        bail!(
            "codesign --display --entitlements failed for {}: {}",
            path.display(),
            stderr.trim()
        );
    }
    tracing::info!(
        target: "ternvale::cli",
        path = %path.display(),
        bytes = stdout.len(),
        hypervisor = entitlement_enabled(&stdout, HYPERVISOR_KEY),
        "dumped code signature entitlements"
    );
    Ok(stdout)
}

/// `codesign --verify --deep --strict` on a bundle or Mach-O.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn verify_deep_strict(path: &Path) -> Result<()> {
    let output = Command::new("codesign")
        .args(["--verify", "--deep", "--strict", "--verbose=2"])
        .arg(path)
        .output()
        .context("run codesign --verify --deep --strict")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        tracing::error!(
            target: "ternvale::cli",
            path = %path.display(),
            stderr = %stderr.trim(),
            "codesign --verify --deep --strict failed"
        );
        bail!(
            "codesign --verify --deep --strict failed for {}: {}",
            path.display(),
            stderr.trim()
        );
    }
    tracing::info!(
        target: "ternvale::cli",
        path = %path.display(),
        "codesign --verify --deep --strict ok"
    );
    Ok(())
}

/// Gatekeeper assess (`spctl -a -vvv --type execute`).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn spctl_assess(path: &Path) -> Result<String> {
    let output = Command::new("spctl")
        .args(["-a", "-vvv", "--type", "execute"])
        .arg(path)
        .output()
        .context("run spctl -a -vvv")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        tracing::info!(target: "ternvale::cli", path = %path.display(), "spctl -a -vvv accepted");
        Ok(text)
    } else {
        tracing::warn!(
            target: "ternvale::cli",
            path = %path.display(),
            status = ?output.status.code(),
            text = %text.trim(),
            "spctl -a -vvv rejected"
        );
        bail!("spctl -a -vvv rejected {}: {}", path.display(), text.trim());
    }
}

/// Check app + embedded helper signatures and hypervisor entitlements.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(app = %app.display(), expect_gatekeeper))]
pub fn release_verify(app: &Path, expect_gatekeeper: bool) -> Result<std::process::ExitCode> {
    if !app.is_dir() {
        bail!("{} is not an .app bundle", app.display());
    }
    let helper = helper_path(app);
    verify_deep_strict(app)?;
    if helper.is_file() {
        verify_deep_strict(&helper)?;
    } else {
        tracing::warn!(
            target: "ternvale::cli",
            path = %helper.display(),
            "embedded helper missing; skipped helper verify"
        );
    }

    let app_exe = macos_executable(app)?;
    let app_ent = dump_entitlements(&app_exe)?;
    require_hypervisor(&app_exe, &app_ent)?;
    if helper.is_file() {
        let helper_ent = dump_entitlements(&helper)?;
        require_hypervisor(&helper, &helper_ent)?;
    }

    match spctl_assess(app) {
        Ok(text) => {
            println!("{}", text.trim());
        }
        Err(error) if !expect_gatekeeper => {
            tracing::warn!(
                target: "ternvale::cli",
                error = %error,
                "spctl skipped as a hard failure (notarization did not run)"
            );
            eprintln!("warning: {error}");
            eprintln!(
                "warning: Gatekeeper will pass only after Developer ID + notarytool + staple"
            );
        }
        Err(error) => return Err(error),
    }

    println!("release-verify: hypervisor entitlement present on app and helper");
    Ok(std::process::ExitCode::SUCCESS)
}

fn helper_path(app: &Path) -> PathBuf {
    app.join("Contents/Helpers/ternvale")
}

fn macos_executable(app: &Path) -> Result<PathBuf> {
    let exe = app.join("Contents/MacOS/Ternvale");
    if exe.is_file() {
        Ok(exe)
    } else {
        bail!("missing {}", exe.display())
    }
}

fn require_hypervisor(path: &Path, plist: &str) -> Result<()> {
    if entitlement_enabled(plist, HYPERVISOR_KEY) {
        tracing::info!(
            target: "ternvale::cli",
            path = %path.display(),
            "hypervisor entitlement present"
        );
        Ok(())
    } else {
        tracing::error!(
            target: "ternvale::cli",
            path = %path.display(),
            "hypervisor entitlement missing after sign"
        );
        bail!(
            "{} is missing {HYPERVISOR_KEY} after signing",
            path.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>com.apple.security.hypervisor</key>
	<true/>
	<key>com.apple.vm.device-access</key>
	<true/>
</dict>
</plist>
"#;

    #[test]
    fn reads_hypervisor_and_device_access() {
        assert!(entitlement_enabled(SAMPLE, HYPERVISOR_KEY));
        assert!(entitlement_enabled(SAMPLE, DEVICE_ACCESS_KEY));
        assert!(!entitlement_enabled(
            SAMPLE,
            "com.apple.security.get-task-allow"
        ));
    }

    #[test]
    fn false_value_is_not_enabled() {
        let xml = "<key>com.apple.security.hypervisor</key>\n\t<false/>\n";
        assert!(!entitlement_enabled(xml, HYPERVISOR_KEY));
    }
}
