//! macOS restore-image / installer operations (VZ only; `guest_macos`).

use std::path::Path;

use ternvale_vz::{DownloadResult, MacosIdentity, MacosRestoreInfo};

use crate::BackendError;

/// Inspect a user IPSW, or Apple's latest supported restore image when `ipsw` is `None`.
#[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(ipsw = ?ipsw))]
pub fn inspect_restore(ipsw: Option<&Path>) -> Result<MacosRestoreInfo, BackendError> {
    ternvale_vz::inspect(ipsw).map_err(BackendError::from)
}

/// Fail if this Mac cannot run `info`.
#[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
pub fn require_supported(info: &MacosRestoreInfo) -> Result<(), BackendError> {
    if info.supported && info.hardware_supported {
        return Ok(());
    }
    let detail = if info.reason.is_empty() {
        "this Mac cannot run the restore image (mostFeaturefulSupportedConfiguration is nil or hardwareModel.isSupported is false)"
            .to_string()
    } else {
        info.reason.clone()
    };
    tracing::warn!(
        target: "ternvale::backend",
        reason = %detail,
        capability = "guest_macos",
        "macos restore not supported on host"
    );
    Err(BackendError::UnsupportedOption {
        option: format!("restore image ({detail})"),
        capability: "guest_macos",
    })
}

/// Download with resume and SHA-256 into the user cache.
#[tracing::instrument(level = "info", target = "ternvale::backend", skip_all, fields(dest = %dest.display()))]
pub fn download_restore(
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    progress: impl Fn(u64, u64, u32) + Send + 'static,
) -> Result<DownloadResult, BackendError> {
    ternvale_vz::download(url, dest, expected_sha256, progress).map_err(BackendError::from)
}

/// Hex of a new `VZMacMachineIdentifier` for clones.
#[tracing::instrument(level = "info", target = "ternvale::backend", skip_all)]
pub fn new_machine_identifier() -> Result<String, BackendError> {
    ternvale_vz::new_machine_identifier().map_err(BackendError::from)
}

/// Copy hardware model, mint a new machine identifier, copy AuxiliaryStorage.
#[tracing::instrument(level = "info", target = "ternvale::backend", skip_all, fields(src = %src.display(), dest = %dest.display()))]
pub fn clone_macos_platform(src: &Path, dest: &Path) -> Result<MacosIdentity, BackendError> {
    let src_id = MacosIdentity::load(&MacosIdentity::path_in(src)).map_err(BackendError::from)?;
    let hex = new_machine_identifier()?;
    let new_bytes = ternvale_vz::parse_hex(&hex).map_err(BackendError::from)?;
    if new_bytes == src_id.machine_identifier {
        return Err(BackendError::Io {
            op: "macos_clone",
            detail: "new VZMacMachineIdentifier matched the source (refusing to duplicate)".into(),
        });
    }
    let dest_id = MacosIdentity::new(src_id.hardware_model.clone(), new_bytes);
    dest_id
        .save(&MacosIdentity::path_in(dest))
        .map_err(BackendError::from)?;
    let src_aux = MacosIdentity::aux_path_in(src);
    let dest_aux = MacosIdentity::aux_path_in(dest);
    if src_aux.is_file() {
        std::fs::copy(&src_aux, &dest_aux).map_err(|error| BackendError::Io {
            op: "macos_clone_aux",
            detail: format!("{} -> {}: {error}", src_aux.display(), dest_aux.display()),
        })?;
    }
    tracing::info!(
        target: "ternvale::macos",
        src = %src.display(),
        dest = %dest.display(),
        hw = dest_id.hardware_model.len(),
        "cloned macos platform with new machineIdentifier"
    );
    Ok(dest_id)
}

/// Write `macos-platform.tvmp` + AuxiliaryStorage in `bundle`.
#[tracing::instrument(level = "info", target = "ternvale::backend", skip_all, fields(bundle = %bundle.display()))]
pub fn persist_platform(
    bundle: &Path,
    info: &MacosRestoreInfo,
) -> Result<MacosIdentity, BackendError> {
    ternvale_vz::write_platform(bundle, info).map_err(BackendError::from)
}

/// Run `VZMacOSInstaller`.
#[tracing::instrument(level = "info", target = "ternvale::backend", skip_all)]
pub fn install_macos(
    ipsw: &Path,
    disk: &Path,
    identity: &MacosIdentity,
    aux: &Path,
    cpus: u32,
    ram_bytes: u64,
    progress: impl Fn(u64, u64, u32) + Send + 'static,
) -> Result<(), BackendError> {
    ternvale_vz::install(ipsw, disk, identity, aux, cpus, ram_bytes, progress)
        .map_err(BackendError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(supported: bool, hardware_supported: bool) -> MacosRestoreInfo {
        MacosRestoreInfo {
            url: "https://example.invalid/a.ipsw".into(),
            build: "24A".into(),
            os: "15.0".into(),
            min_cpus: 2,
            min_ram_bytes: 4 << 30,
            content_length: 10,
            supported,
            hardware_supported,
            hardware_hex: "aa".into(),
            identifier_hex: "bb".into(),
            reason: if supported && hardware_supported {
                String::new()
            } else {
                "hardwareModel.isSupported is false".into()
            },
        }
    }

    #[test]
    fn supported_restore_is_ok() {
        require_supported(&info(true, true)).expect("supported");
    }

    #[test]
    fn unsupported_restore_names_guest_macos() {
        let err = require_supported(&info(true, false)).expect_err("hw");
        let text = err.to_string();
        assert!(text.contains("guest_macos"), "{text}");
        assert!(text.contains("hardwareModel"), "{text}");
    }
}
