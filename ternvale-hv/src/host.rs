//! Host facts read through `sysctlbyname`: the macOS product version and
//! whether this machine supports Hypervisor.framework at all.

use std::ffi::{c_void, CStr};

use crate::error::HvError;
use crate::ffi::sysctlbyname;

/// The host's `kern.osproductversion`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosVersion {
    /// First component, for example 15 in "15.6.1".
    pub major: u32,
    /// Second component (0 when the string has only a major).
    pub minor: u32,
    /// The full string, for example "15.6.1".
    pub text: String,
}

/// Read `kern.osproductversion`.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
pub fn macos_version() -> Result<MacosVersion, HvError> {
    let text = sysctl_string(c"kern.osproductversion")?;
    let Some((major, minor)) = parse_version(&text) else {
        tracing::error!(target: "ternvale::hv", version = %text, "unparsed macOS version");
        return Err(HvError::OsVersion);
    };
    tracing::debug!(target: "ternvale::hv", major, minor, version = %text, "host macOS version");
    Ok(MacosVersion { major, minor, text })
}

/// `kern.hv_support`: 1 when the CPU and kernel support Hypervisor.framework.
/// It is 0 inside most VMs (no nested virtualization) and on unsupported Macs.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
pub fn hypervisor_supported() -> Result<bool, HvError> {
    let value = sysctl_i32(c"kern.hv_support")?;
    tracing::debug!(target: "ternvale::hv", kern_hv_support = value, "host hypervisor support");
    Ok(value == 1)
}

pub(crate) fn parse_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

pub(crate) fn sysctl_string(name: &'static CStr) -> Result<String, HvError> {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: `name` is NUL-terminated. `buf` is writable for `len` bytes and
    // `sysctlbyname` writes at most `len` bytes, then stores the length used.
    let rc = unsafe {
        sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast::<c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    tracing::trace!(target: "ternvale::hv", name = ?name, rc, len, "sysctlbyname");
    if rc != 0 {
        tracing::error!(target: "ternvale::hv", name = ?name, rc, "sysctlbyname failed");
        return Err(HvError::Sysctl {
            name: name.to_string_lossy().into_owned(),
        });
    }
    let bytes = buf.get(..len).unwrap_or(&buf);
    Ok(String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string())
}

fn sysctl_i32(name: &'static CStr) -> Result<i32, HvError> {
    let mut value: i32 = 0;
    let mut len = std::mem::size_of::<i32>();
    // SAFETY: `name` is NUL-terminated and `value` is a writable i32 whose size
    // is passed in `len`; `sysctlbyname` writes at most that many bytes.
    let rc = unsafe {
        sysctlbyname(
            name.as_ptr(),
            (&mut value as *mut i32).cast::<c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    tracing::trace!(target: "ternvale::hv", name = ?name, rc, value, "sysctlbyname");
    if rc != 0 {
        tracing::warn!(target: "ternvale::hv", name = ?name, rc, "sysctlbyname failed");
        return Err(HvError::Sysctl {
            name: name.to_string_lossy().into_owned(),
        });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_host_version_and_hv_support() {
        let version = macos_version().expect("kern.osproductversion");
        assert!(version.major >= 11, "{version:?}");
        assert!(version.text.starts_with(&version.major.to_string()));
        hypervisor_supported().expect("kern.hv_support");
    }

    #[test]
    fn unknown_sysctls_are_errors() {
        let error = sysctl_i32(c"kern.ternvale_nonexistent").expect_err("missing");
        assert!(
            error.to_string().contains("kern.ternvale_nonexistent"),
            "{error}"
        );
    }

    #[test]
    fn parses_a_product_version() {
        assert_eq!(parse_version("15.6.1"), Some((15, 6)));
        assert_eq!(parse_version("27.0"), Some((27, 0)));
        assert_eq!(parse_version("15"), Some((15, 0)));
        assert_eq!(parse_version(""), None);
    }
}
