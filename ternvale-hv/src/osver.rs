//! Host macOS version for the in-kernel GICv3 availability check.
//!
//! `hv_gic.h` in the macOS SDK marks `hv_gic_create` and the other GICv3 calls
//! `API_AVAILABLE(macos(15.0))`. macOS 15 is the minimum.

use std::ffi::c_void;
#[cfg(test)]
use std::sync::Mutex;

use crate::error::HvError;
use crate::ffi::sysctlbyname;

/// Major version required by `API_AVAILABLE(macos(15.0))`.
pub const GIC_MIN_MACOS_MAJOR: u32 = 15;

#[cfg(test)]
static OVERRIDE: Mutex<Option<(u32, u32)>> = Mutex::new(None);

/// Replace the host version for tests. `None` reads `kern.osproductversion`.
#[cfg(test)]
pub fn set_gic_os_version_override(version: Option<(u32, u32)>) {
    let mut slot = OVERRIDE.lock().unwrap_or_else(|poison| poison.into_inner());
    *slot = version;
}

/// Accept macOS 15.0 and any later release.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::gic",
    skip_all,
    fields(major, minor)
)]
pub fn ensure_gic_os_version(major: u32, minor: u32) -> Result<(), HvError> {
    tracing::Span::current().record("major", major);
    tracing::Span::current().record("minor", minor);
    if major >= GIC_MIN_MACOS_MAJOR {
        tracing::debug!(
            target: "ternvale::gic",
            major,
            minor,
            "host supports in-kernel GICv3"
        );
        return Ok(());
    }
    tracing::error!(
        target: "ternvale::gic",
        major,
        minor,
        required = GIC_MIN_MACOS_MAJOR,
        "in-kernel GICv3 is not available on this macOS version"
    );
    Err(HvError::GicOsUnsupported { major, minor })
}

/// Read the host version, or the test override, and require macOS 15.0.
#[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all)]
pub fn ensure_gic_os() -> Result<(), HvError> {
    let (major, minor) = host_version()?;
    ensure_gic_os_version(major, minor)
}

fn host_version() -> Result<(u32, u32), HvError> {
    #[cfg(test)]
    if let Some(version) = *OVERRIDE.lock().unwrap_or_else(|poison| poison.into_inner()) {
        return Ok(version);
    }
    let mut buf = [0u8; 32];
    let mut len = buf.len();
    let name = c"kern.osproductversion";
    // SAFETY: `name` is a NUL-terminated literal. `buf` is writable for `len` bytes.
    // `sysctlbyname` writes the version string and updates `len`.
    let rc = unsafe {
        sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast::<c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        tracing::error!(target: "ternvale::gic", rc, "sysctlbyname kern.osproductversion failed");
        return Err(HvError::OsVersion);
    }
    let text = std::str::from_utf8(&buf[..len]).unwrap_or("");
    let text = text.trim_end_matches('\0').trim();
    parse_version(text).ok_or_else(|| {
        tracing::error!(target: "ternvale::gic", version = text, "unparsed macOS version");
        HvError::OsVersion
    })
}

fn parse_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_gic_os, ensure_gic_os_version, parse_version, set_gic_os_version_override,
        GIC_MIN_MACOS_MAJOR,
    };
    use crate::HvError;

    #[test]
    fn macos_15_is_the_documented_minimum() {
        assert_eq!(GIC_MIN_MACOS_MAJOR, 15);
    }

    #[test]
    fn rejects_macos_14_and_accepts_15_and_later() {
        let old = ensure_gic_os_version(14, 6).expect_err("14.6");
        assert!(matches!(
            old,
            HvError::GicOsUnsupported {
                major: 14,
                minor: 6
            }
        ));
        assert!(old.to_string().contains("15.0"), "{old}");
        ensure_gic_os_version(15, 0).expect("15.0");
        ensure_gic_os_version(27, 0).expect("27.0");
    }

    #[test]
    fn an_injected_old_version_is_rejected() {
        set_gic_os_version_override(Some((14, 2)));
        let error = ensure_gic_os().expect_err("14.2");
        set_gic_os_version_override(None);
        assert!(matches!(
            error,
            HvError::GicOsUnsupported {
                major: 14,
                minor: 2
            }
        ));
        assert!(error.to_string().contains("15.0"), "{error}");
    }

    #[test]
    fn parses_a_product_version() {
        assert_eq!(parse_version("15.6.1"), Some((15, 6)));
        assert_eq!(parse_version("27.0"), Some((27, 0)));
        assert_eq!(parse_version("15"), Some((15, 0)));
        assert_eq!(parse_version(""), None);
    }
}
