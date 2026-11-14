//! C ABI wrappers for restore-image inspect, IPSW download, and VZMacOSInstaller.

use std::collections::BTreeMap;
use std::os::raw::{c_char, c_void};
use std::path::Path;

use crate::error::VzError;
use crate::ffi::{cstr_owned, cstring, ensure_logger, read_err};
use crate::macos::{parse_hex, MacosIdentity};

const ERR_LEN: usize = 512;
const OUT_LEN: usize = 16 * 1024;

extern "C" {
    fn tv_vz_macos_inspect(
        ipsw: *const c_char,
        out: *mut c_char,
        out_len: usize,
        err: *mut c_char,
        err_len: usize,
    ) -> i32;
    fn tv_vz_macos_download(
        url: *const c_char,
        dest: *const c_char,
        expected_sha256: *const c_char,
        cb: Option<extern "C" fn(*mut c_void, u64, u64, u32)>,
        ctx: *mut c_void,
        out: *mut c_char,
        out_len: usize,
        err: *mut c_char,
        err_len: usize,
    ) -> i32;
    fn tv_vz_macos_create_aux(
        aux: *const c_char,
        hw_hex: *const c_char,
        err: *mut c_char,
        err_len: usize,
    ) -> i32;
    fn tv_vz_macos_new_machine_id(
        out: *mut c_char,
        out_len: usize,
        err: *mut c_char,
        err_len: usize,
    ) -> i32;
    fn tv_vz_macos_install(
        ipsw: *const c_char,
        disk: *const c_char,
        aux: *const c_char,
        hw_hex: *const c_char,
        id_hex: *const c_char,
        cpus: u32,
        ram_bytes: u64,
        cb: Option<extern "C" fn(*mut c_void, u64, u64, u32)>,
        ctx: *mut c_void,
        err: *mut c_char,
        err_len: usize,
    ) -> i32;
}

/// Restore image metadata from Virtualization.framework.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosRestoreInfo {
    pub url: String,
    pub build: String,
    pub os: String,
    pub min_cpus: u32,
    pub min_ram_bytes: u64,
    pub content_length: u64,
    pub supported: bool,
    pub hardware_supported: bool,
    pub hardware_hex: String,
    pub identifier_hex: String,
    pub reason: String,
}

impl MacosRestoreInfo {
    /// Catalog size, or 16 GiB when Apple did not advertise Content-Length.
    #[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
    pub fn restore_bytes(&self) -> u64 {
        if self.content_length > 0 {
            self.content_length
        } else {
            16 << 30
        }
    }
}

/// Result of [`download`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadResult {
    pub sha256: String,
    pub bytes: u64,
    pub resumed: bool,
}

struct ProgressHook {
    cb: Box<dyn Fn(u64, u64, u32) + Send>,
}

/// Inspect a local IPSW, or fetch the latest supported image when `ipsw` is `None`.
#[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(ipsw = ?ipsw))]
pub fn inspect(ipsw: Option<&Path>) -> Result<MacosRestoreInfo, VzError> {
    ensure_logger();
    let path = ipsw.map(|p| p.display().to_string()).unwrap_or_default();
    let c_path = cstring(&path)?;
    let mut out = vec![0 as c_char; OUT_LEN];
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: C strings and buffers live for the call.
    let rc = unsafe {
        tv_vz_macos_inspect(
            c_path.as_ptr(),
            out.as_mut_ptr(),
            out.len(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    read_err(&err, rc, "macos_inspect")?;
    // SAFETY: `out` is a NUL-terminated buffer filled by Swift for this call.
    let blob = unsafe { cstr_owned(out.as_ptr()) };
    let info = parse_info(&blob)?;
    tracing::info!(
        target: "ternvale::vz",
        build = %info.build,
        os = %info.os,
        supported = info.supported,
        hardware_supported = info.hardware_supported,
        min_cpus = info.min_cpus,
        min_ram_bytes = info.min_ram_bytes,
        "macos restore image"
    );
    Ok(info)
}

/// Download `url` to `dest` with HTTP resume. `expected_sha256` may be empty.
#[tracing::instrument(
    level = "info",
    target = "ternvale::vz",
    skip_all,
    fields(dest = %dest.display())
)]
pub fn download(
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    progress: impl Fn(u64, u64, u32) + Send + 'static,
) -> Result<DownloadResult, VzError> {
    ensure_logger();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|error| VzError::Io {
            op: "macos_cache",
            detail: error.to_string(),
        })?;
    }
    let c_url = cstring(url)?;
    let c_dest = cstring(&dest.display().to_string())?;
    let c_sha = cstring(expected_sha256.unwrap_or(""))?;
    let mut hook = ProgressHook {
        cb: Box::new(progress),
    };
    let ctx = std::ptr::from_mut(&mut hook).cast::<c_void>();
    let mut out = vec![0 as c_char; OUT_LEN];
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: `hook` is pinned on this stack until after the call returns.
    let rc = unsafe {
        tv_vz_macos_download(
            c_url.as_ptr(),
            c_dest.as_ptr(),
            c_sha.as_ptr(),
            Some(progress_trampoline),
            ctx,
            out.as_mut_ptr(),
            out.len(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    read_err(&err, rc, "macos_download")?;
    // SAFETY: `out` is a NUL-terminated buffer filled by Swift for this call.
    let blob = unsafe { cstr_owned(out.as_ptr()) };
    let kv = parse_map(&blob);
    Ok(DownloadResult {
        sha256: kv.get("sha256").cloned().unwrap_or_default(),
        bytes: kv.get("bytes").and_then(|s| s.parse().ok()).unwrap_or(0),
        resumed: kv.get("resumed").map(|s| s == "1").unwrap_or(false),
    })
}

/// Create `VZMacAuxiliaryStorage` at `aux` for `hardware_model` hex.
#[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(aux = %aux.display()))]
pub fn create_aux(aux: &Path, hardware_hex: &str) -> Result<(), VzError> {
    ensure_logger();
    if let Some(parent) = aux.parent() {
        std::fs::create_dir_all(parent).map_err(|error| VzError::Io {
            op: "macos_aux",
            detail: error.to_string(),
        })?;
    }
    let c_aux = cstring(&aux.display().to_string())?;
    let c_hw = cstring(hardware_hex)?;
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: C strings and `err` live for the call.
    let rc = unsafe {
        tv_vz_macos_create_aux(c_aux.as_ptr(), c_hw.as_ptr(), err.as_mut_ptr(), err.len())
    };
    read_err(&err, rc, "macos_aux")
}

/// Hex of a new `VZMacMachineIdentifier` (clone; never reuse the source).
#[tracing::instrument(level = "info", target = "ternvale::vz", skip_all)]
pub fn new_machine_identifier() -> Result<String, VzError> {
    ensure_logger();
    let mut out = [0 as c_char; ERR_LEN];
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: buffers live for the call; Swift writes a hex C string.
    let rc = unsafe {
        tv_vz_macos_new_machine_id(out.as_mut_ptr(), out.len(), err.as_mut_ptr(), err.len())
    };
    read_err(&err, rc, "macos_new_id")?;
    // SAFETY: NUL-terminated hex written by Swift.
    let hex = unsafe { cstr_owned(out.as_ptr()) };
    if hex.is_empty() {
        return Err(VzError::Host {
            op: "macos_new_id",
            detail: "empty machineIdentifier".into(),
        });
    }
    tracing::info!(
        target: "ternvale::vz",
        bytes = hex.len() / 2,
        "generated macos machine identifier for clone"
    );
    Ok(hex)
}

/// Persist identity + aux. Reuses an existing identity so the machine id stays stable.
#[tracing::instrument(level = "info", target = "ternvale::vz", skip_all, fields(bundle = %bundle.display()))]
pub fn write_platform(bundle: &Path, info: &MacosRestoreInfo) -> Result<MacosIdentity, VzError> {
    let path = MacosIdentity::path_in(bundle);
    if path.is_file() {
        let existing = MacosIdentity::load(&path)?;
        tracing::info!(
            target: "ternvale::vz",
            path = %path.display(),
            "reusing macos platform identity"
        );
        return Ok(existing);
    }
    if !info.supported || !info.hardware_supported {
        return Err(VzError::Host {
            op: "macos_support",
            detail: if info.reason.is_empty() {
                "this Mac cannot run the restore image (hardwareModel.isSupported is false or mostFeaturefulSupportedConfiguration is nil)".into()
            } else {
                info.reason.clone()
            },
        });
    }
    if info.identifier_hex.is_empty() || info.hardware_hex.is_empty() {
        return Err(VzError::Host {
            op: "macos_platform",
            detail: "restore image did not return hardwareModel and machineIdentifier".into(),
        });
    }
    let identity = MacosIdentity::new(
        parse_hex(&info.hardware_hex)?,
        parse_hex(&info.identifier_hex)?,
    );
    identity.save(&path)?;
    let aux = MacosIdentity::aux_path_in(bundle);
    if !aux.is_file() {
        create_aux(&aux, &identity.hardware_hex())?;
    }
    Ok(identity)
}

/// Run `VZMacOSInstaller` (blocking) with progress callbacks.
#[tracing::instrument(
    level = "info",
    target = "ternvale::vz",
    skip_all,
    fields(ipsw = %ipsw.display(), disk = %disk.display())
)]
pub fn install(
    ipsw: &Path,
    disk: &Path,
    identity: &MacosIdentity,
    aux: &Path,
    cpus: u32,
    ram_bytes: u64,
    progress: impl Fn(u64, u64, u32) + Send + 'static,
) -> Result<(), VzError> {
    ensure_logger();
    let c_ipsw = cstring(&ipsw.display().to_string())?;
    let c_disk = cstring(&disk.display().to_string())?;
    let c_aux = cstring(&aux.display().to_string())?;
    let c_hw = cstring(&identity.hardware_hex())?;
    let c_id = cstring(&identity.identifier_hex())?;
    let mut hook = ProgressHook {
        cb: Box::new(progress),
    };
    let ctx = std::ptr::from_mut(&mut hook).cast::<c_void>();
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: C strings, `hook`/`ctx`, and `err` live until this call returns.
    let rc = unsafe {
        tv_vz_macos_install(
            c_ipsw.as_ptr(),
            c_disk.as_ptr(),
            c_aux.as_ptr(),
            c_hw.as_ptr(),
            c_id.as_ptr(),
            cpus,
            ram_bytes,
            Some(progress_trampoline),
            ctx,
            err.as_mut_ptr(),
            err.len(),
        )
    };
    read_err(&err, rc, "macos_install")?;
    tracing::info!(target: "ternvale::vz", "macos installer finished");
    Ok(())
}

extern "C" fn progress_trampoline(ctx: *mut c_void, completed: u64, total: u64, permille: u32) {
    // SAFETY: ctx is `&mut ProgressHook` from download/install, live until return.
    let hook = unsafe { &mut *ctx.cast::<ProgressHook>() };
    (hook.cb)(completed, total, permille);
}

fn parse_info(blob: &str) -> Result<MacosRestoreInfo, VzError> {
    let kv = parse_map(blob);
    Ok(MacosRestoreInfo {
        url: kv.get("url").cloned().unwrap_or_default(),
        build: kv.get("build").cloned().unwrap_or_default(),
        os: kv.get("os").cloned().unwrap_or_default(),
        min_cpus: kv.get("min_cpus").and_then(|s| s.parse().ok()).unwrap_or(0),
        min_ram_bytes: kv
            .get("min_ram_bytes")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        content_length: kv
            .get("content_length")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        supported: kv.get("supported").map(|s| s == "1").unwrap_or(false),
        hardware_supported: kv
            .get("hardware_supported")
            .map(|s| s == "1")
            .unwrap_or(false),
        hardware_hex: kv.get("hw").cloned().unwrap_or_default(),
        identifier_hex: kv.get("id").cloned().unwrap_or_default(),
        reason: kv.get("reason").cloned().unwrap_or_default(),
    })
}

fn parse_map(blob: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in blob.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

#[cfg(test)]
#[path = "macos_download_tests.rs"]
mod macos_download_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kv_restore_info() {
        let info = parse_info(
            "url=https://example/a.ipsw\nbuild=24A\nos=15.0\nmin_cpus=2\nmin_ram_bytes=4294967296\ncontent_length=10\nsupported=1\nhardware_supported=1\nhw=aa\nid=bb\nreason=\n",
        )
        .expect("parse");
        assert_eq!(info.build, "24A");
        assert!(info.supported);
        assert_eq!(info.restore_bytes(), 10);
    }
}
