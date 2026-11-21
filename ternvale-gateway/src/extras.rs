//! Screenshot (PNG) and serial-log helpers. Display streaming is out of scope.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::GatewayError;
use crate::png;

/// Framebuffer dump magic (`TVFB` v1).
pub const FB_MAGIC: &[u8; 4] = b"TVFB";
/// Current framebuffer version.
pub const FB_VERSION: u32 = 1;

/// Locate `{run_dir}/{name}.fb`.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(name))]
pub fn framebuffer_path(run_dir: &Path, name: &str) -> PathBuf {
    run_dir.join(format!("{name}.fb"))
}

/// Encode a PNG from a `TVFB` v1 dump, or a 1×1 placeholder if missing.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(path = %path.display()))]
pub fn screenshot_png(path: &Path) -> Result<(Vec<u8>, bool), GatewayError> {
    if !path.is_file() {
        tracing::debug!(
            target: "ternvale::gateway",
            path = %path.display(),
            "no framebuffer dump; placeholder png"
        );
        return Ok((png::placeholder()?, false));
    }
    let bytes = std::fs::read(path)
        .map_err(|source| GatewayError::io_path("read framebuffer", path.display(), source))?;
    if bytes.len() < 16 {
        return Err(GatewayError::Version("framebuffer dump too short".into()));
    }
    if &bytes[0..4] != FB_MAGIC {
        return Err(GatewayError::Version(
            "framebuffer magic is not TVFB".into(),
        ));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or([0; 4]));
    if version != FB_VERSION {
        return Err(GatewayError::Version(format!(
            "unsupported framebuffer version {version} (want {FB_VERSION})"
        )));
    }
    let width = u32::from_le_bytes(bytes[8..12].try_into().unwrap_or([0; 4]));
    let height = u32::from_le_bytes(bytes[12..16].try_into().unwrap_or([0; 4]));
    let rgba = &bytes[16..];
    let need = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| GatewayError::Http("framebuffer overflow".into()))?;
    if rgba.len() != need {
        return Err(GatewayError::Http(
            "framebuffer pixel length mismatch".into(),
        ));
    }
    let mut rgb = Vec::with_capacity(need / 4 * 3);
    for chunk in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&chunk[..3]);
    }
    Ok((png::encode_rgb(width, height, &rgb)?, true))
}

/// `serial_log` from a library VM TOML, if present.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(name))]
pub fn serial_log_path(vms_dir: &Path, name: &str) -> Option<PathBuf> {
    let config = vms_dir.join(name).join("config.toml");
    let text = std::fs::read_to_string(&config).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("serial_log")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

/// Follow `path` and invoke `on_chunk` for new bytes until it returns false.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(path = %path.display()))]
pub fn follow_file(
    path: &Path,
    mut on_chunk: impl FnMut(&[u8]) -> Result<bool, GatewayError>,
) -> Result<(), GatewayError> {
    let mut file = File::open(path)
        .map_err(|source| GatewayError::io_path("open serial log", path.display(), source))?;
    file.seek(SeekFrom::End(0))
        .map_err(|source| GatewayError::io_path("seek serial log", path.display(), source))?;
    let mut buf = [0u8; 4096];
    loop {
        match file.read(&mut buf) {
            Ok(0) => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Ok(n) => {
                if !on_chunk(&buf[..n])? {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(GatewayError::io_path(
                    "read serial log",
                    path.display(),
                    error,
                ));
            }
        }
    }
}

/// Default `~/Library/Application Support/Ternvale/vms`.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn default_vms_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TERNVALE_VMS_DIR") {
        return PathBuf::from(dir);
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join("Library/Application Support/Ternvale/vms"),
        None => PathBuf::from("/tmp/ternvale-vms"),
    }
}

/// Default run directory.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn default_run_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TERNVALE_RUN_DIR") {
        return PathBuf::from(dir);
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join("Library/Application Support/Ternvale/run"),
        None => PathBuf::from("/tmp/ternvale-run"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tvfb_round_trip_png() {
        let dir = std::env::temp_dir().join(format!("tvfb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("vm.fb");
        let mut dump = Vec::from(*FB_MAGIC);
        dump.extend_from_slice(&FB_VERSION.to_le_bytes());
        dump.extend_from_slice(&2u32.to_le_bytes());
        dump.extend_from_slice(&1u32.to_le_bytes());
        dump.extend_from_slice(&[255, 0, 0, 255, 0, 255, 0, 255]);
        std::fs::write(&path, dump).expect("write");
        let (png, present) = screenshot_png(&path).expect("shot");
        assert!(present);
        assert!(crate::png::is_png(&png));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
