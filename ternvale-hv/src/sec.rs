//! Security.framework: code-signing checks and security-scoped bookmarks.
//!
//! Raw FFI stays here. Other crates call [`check_pid_requirement`],
//! [`create_security_scoped_bookmark`], and [`resolve_security_scoped_bookmark`].

use std::ffi::{c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr;

use thiserror::Error;

use super::ffi;

/// `kSecGuestAttributePid` as a CF string (Security.framework).
const GUEST_ATTR_PID: &CStr = c"pid";
/// `kCFURLBookmarkCreationWithSecurityScope` (1 << 11).
const BOOKMARK_CREATE_SCOPE: usize = 1 << 11;
/// `kCFURLBookmarkResolutionWithSecurityScope` (1 << 10).
const BOOKMARK_RESOLVE_SCOPE: usize = 1 << 10;
const PATH_STYLE_POSIX: usize = 0;
const STRING_UTF8: u32 = 0x0800_0100;
const TRUE: u8 = 1;

/// Codesign or bookmark failure.
#[derive(Debug, Error)]
pub enum SecError {
    /// `SecRequirementCreateWithString` rejected the text.
    #[error("codesign requirement is not valid: {0}")]
    BadRequirement(String),
    /// Guest code object could not be created for `pid`.
    #[error("no code signature for pid {pid}: {detail}")]
    NoCode {
        /// Process id.
        pid: u32,
        /// Security.framework status text.
        detail: String,
    },
    /// `SecCodeCheckValidity` failed.
    #[error("pid {pid} failed the codesign requirement: {detail}")]
    Check {
        /// Process id.
        pid: u32,
        /// Status text.
        detail: String,
    },
    /// Bookmark create/resolve failed.
    #[error("security-scoped bookmark: {0}")]
    Bookmark(String),
}

/// Compile `text` as a `SecRequirement` (does not evaluate a process).
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
pub fn parse_requirement(text: &str) -> Result<(), SecError> {
    let req = requirement_from_text(text)?;
    // SAFETY: `req` came from SecRequirementCreateWithString.
    unsafe { cf_release(req) };
    Ok(())
}

/// True when `pid` satisfies `requirement` (Code Signing Language).
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(pid))]
pub fn check_pid_requirement(pid: u32, requirement: &str) -> Result<(), SecError> {
    let req = requirement_from_text(requirement)?;
    let guest = guest_for_pid(pid)?;
    // SAFETY: `guest` and `req` are live Security objects for this check.
    let status = unsafe { ffi::SecCodeCheckValidity(guest, 0, req) };
    tracing::trace!(
        target: "ternvale::hv",
        pid,
        status,
        "SecCodeCheckValidity"
    );
    // SAFETY: both objects were created in this function.
    unsafe {
        cf_release(guest);
        cf_release(req);
    }
    if status != 0 {
        let detail = osstatus_text(status);
        tracing::warn!(
            target: "ternvale::hv",
            pid,
            status,
            detail = %detail,
            "caller failed codesign requirement"
        );
        return Err(SecError::Check { pid, detail });
    }
    tracing::debug!(target: "ternvale::hv", pid, "caller satisfied codesign requirement");
    Ok(())
}

/// Create a security-scoped bookmark blob for `path`.
///
/// TODO(verify): creating a scoped bookmark from a non-App-Sandbox process
/// may yield a blob that only the same user can resolve; confirm on macOS 15–26.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(path = %path.display()))]
pub fn create_security_scoped_bookmark(path: &Path) -> Result<Vec<u8>, SecError> {
    let url = cf_url_from_path(path)?;
    let mut err: *mut c_void = ptr::null_mut();
    // SAFETY: `url` is a CFURL; out-error is a nullable CFError**.
    let data = unsafe {
        ffi::CFURLCreateBookmarkData(
            ptr::null_mut(),
            url,
            BOOKMARK_CREATE_SCOPE,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut err,
        )
    };
    // SAFETY: `url` was created above.
    unsafe { cf_release(url) };
    if data.is_null() {
        let detail = cf_error_text(err);
        tracing::warn!(
            target: "ternvale::hv",
            path = %path.display(),
            detail = %detail,
            "CFURLCreateBookmarkData failed"
        );
        return Err(SecError::Bookmark(detail));
    }
    let bytes = cf_data_bytes(data);
    // SAFETY: `data` is a CFData from CreateBookmarkData.
    unsafe { cf_release(data) };
    tracing::debug!(
        target: "ternvale::hv",
        path = %path.display(),
        bytes = bytes.len(),
        "created security-scoped bookmark"
    );
    Ok(bytes)
}

/// Resolve a scoped bookmark and start accessing the resource. Stop with [`ScopedAccess`].
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
pub fn resolve_security_scoped_bookmark(blob: &[u8]) -> Result<ScopedAccess, SecError> {
    let data = cf_data_from_bytes(blob)?;
    let mut stale: u8 = 0;
    let mut err: *mut c_void = ptr::null_mut();
    // SAFETY: `data` is CFData holding the bookmark; flags request security scope.
    let url = unsafe {
        ffi::CFURLCreateByResolvingBookmarkData(
            ptr::null_mut(),
            data,
            BOOKMARK_RESOLVE_SCOPE,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut stale,
            &raw mut err,
        )
    };
    // SAFETY: `data` created above.
    unsafe { cf_release(data) };
    if url.is_null() {
        return Err(SecError::Bookmark(cf_error_text(err)));
    }
    // SAFETY: `url` is a resolved file URL.
    let started = unsafe { ffi::CFURLStartAccessingSecurityScopedResource(url) } != 0;
    tracing::debug!(
        target: "ternvale::hv",
        started,
        stale = stale != 0,
        "resolved security-scoped bookmark"
    );
    if !started {
        // SAFETY: still own `url`.
        unsafe { cf_release(url) };
        return Err(SecError::Bookmark(
            "CFURLStartAccessingSecurityScopedResource returned false".into(),
        ));
    }
    let path = cf_url_path(url)?;
    Ok(ScopedAccess { url, path })
}

/// Live security-scoped access; stopping happens on drop.
pub struct ScopedAccess {
    url: *mut c_void,
    path: PathBuf,
}

// SAFETY: CFURL is a CFType; we only use it from the creating thread for start/stop.
unsafe impl Send for ScopedAccess {}

impl ScopedAccess {
    /// Host path the bookmark resolved to.
    #[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScopedAccess {
    fn drop(&mut self) {
        if !self.url.is_null() {
            // SAFETY: `url` is the CFURL we started accessing.
            unsafe {
                ffi::CFURLStopAccessingSecurityScopedResource(self.url);
                cf_release(self.url);
            }
            self.url = ptr::null_mut();
        }
    }
}

fn requirement_from_text(text: &str) -> Result<*mut c_void, SecError> {
    let c = CString::new(text).map_err(|_| SecError::BadRequirement("embedded NUL".into()))?;
    // SAFETY: UTF-8 C string, no-copy, lives for the Create call.
    let cf = unsafe { ffi::CFStringCreateWithCString(ptr::null_mut(), c.as_ptr(), STRING_UTF8) };
    if cf.is_null() {
        return Err(SecError::BadRequirement("CFStringCreateWithCString".into()));
    }
    let mut req = ptr::null_mut();
    // SAFETY: `cf` is a CFString; `req` is an out-param.
    let status = unsafe { ffi::SecRequirementCreateWithString(cf, 0, &raw mut req) };
    tracing::trace!(
        target: "ternvale::hv",
        status,
        "SecRequirementCreateWithString"
    );
    // SAFETY: `cf` created above.
    unsafe { cf_release(cf) };
    if status != 0 || req.is_null() {
        return Err(SecError::BadRequirement(osstatus_text(status)));
    }
    Ok(req)
}

fn guest_for_pid(pid: u32) -> Result<*mut c_void, SecError> {
    let key = cf_string(GUEST_ATTR_PID)?;
    let num = unsafe {
        // SAFETY: integer CFNumber for the pid.
        ffi::CFNumberCreate(
            ptr::null_mut(),
            /* kCFNumberSInt32Type */ 3,
            (&raw const pid).cast(),
        )
    };
    if num.is_null() {
        unsafe { cf_release(key) };
        return Err(SecError::NoCode {
            pid,
            detail: "CFNumberCreate".into(),
        });
    }
    // SAFETY: CoreFoundation exports the CFType dictionary callbacks.
    let dict = unsafe {
        ffi::CFDictionaryCreateMutable(
            ptr::null_mut(),
            1,
            &raw const ffi::kCFTypeDictionaryKeyCallBacks,
            &raw const ffi::kCFTypeDictionaryValueCallBacks,
        )
    };
    if dict.is_null() {
        unsafe {
            cf_release(key);
            cf_release(num);
        }
        return Err(SecError::NoCode {
            pid,
            detail: "CFDictionaryCreateMutable".into(),
        });
    }
    // SAFETY: `dict` is mutable; key/value are CFString/CFNumber.
    unsafe { ffi::CFDictionarySetValue(dict, key, num) };
    unsafe {
        cf_release(key);
        cf_release(num);
    }
    let mut guest = ptr::null_mut();
    // SAFETY: `dict` holds kSecGuestAttributePid; `guest` is an out-param.
    let status =
        unsafe { ffi::SecCodeCopyGuestWithAttributes(ptr::null_mut(), dict, 0, &raw mut guest) };
    tracing::trace!(
        target: "ternvale::hv",
        pid,
        status,
        "SecCodeCopyGuestWithAttributes"
    );
    unsafe { cf_release(dict) };
    if status != 0 || guest.is_null() {
        return Err(SecError::NoCode {
            pid,
            detail: osstatus_text(status),
        });
    }
    Ok(guest)
}

fn cf_url_from_path(path: &Path) -> Result<*mut c_void, SecError> {
    let text = path
        .to_str()
        .ok_or_else(|| SecError::Bookmark("path is not UTF-8".into()))?;
    let c = CString::new(text).map_err(|_| SecError::Bookmark("path contains NUL".into()))?;
    // SAFETY: `c` is a NUL-terminated UTF-8 path; allocator NULL is the default.
    let cf = unsafe { ffi::CFStringCreateWithCString(ptr::null_mut(), c.as_ptr(), STRING_UTF8) };
    if cf.is_null() {
        return Err(SecError::Bookmark("CFStringCreateWithCString".into()));
    }
    // SAFETY: `cf` is a POSIX path string.
    let is_dir = if path.is_dir() { TRUE } else { 0 };
    let url = unsafe {
        ffi::CFURLCreateWithFileSystemPath(ptr::null_mut(), cf, PATH_STYLE_POSIX, is_dir)
    };
    // SAFETY: `cf` is the +1 string we just created.
    unsafe { cf_release(cf) };
    if url.is_null() {
        return Err(SecError::Bookmark("CFURLCreateWithFileSystemPath".into()));
    }
    Ok(url)
}

fn cf_url_path(url: *mut c_void) -> Result<PathBuf, SecError> {
    let mut buf = [0i8; 1024];
    // SAFETY: `url` is CFURL; buffer is a POSIX path out-param.
    let ok = unsafe {
        ffi::CFURLGetFileSystemRepresentation(
            url,
            TRUE,
            buf.as_mut_ptr().cast(),
            buf.len() as isize,
        )
    };
    if ok == 0 {
        return Err(SecError::Bookmark(
            "CFURLGetFileSystemRepresentation".into(),
        ));
    }
    // SAFETY: CFURLGetFileSystemRepresentation wrote a NUL-terminated POSIX path.
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Ok(PathBuf::from(cstr.to_string_lossy().into_owned()))
}

fn cf_data_from_bytes(bytes: &[u8]) -> Result<*mut c_void, SecError> {
    if bytes.len() > 1024 * 1024 {
        return Err(SecError::Bookmark("bookmark blob larger than 1 MiB".into()));
    }
    // SAFETY: copy of `bytes` into a CFData.
    let data = unsafe { ffi::CFDataCreate(ptr::null_mut(), bytes.as_ptr(), bytes.len() as isize) };
    if data.is_null() {
        return Err(SecError::Bookmark("CFDataCreate".into()));
    }
    Ok(data)
}

fn cf_data_bytes(data: *mut c_void) -> Vec<u8> {
    // SAFETY: `data` is CFData.
    let len = unsafe { ffi::CFDataGetLength(data) } as usize;
    let ptr = unsafe { ffi::CFDataGetBytePtr(data) };
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    // SAFETY: pointer valid for `len` bytes for the life of `data`.
    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
}

fn cf_string(text: &CStr) -> Result<*mut c_void, SecError> {
    // SAFETY: `text` is a live CStr; default allocator.
    let cf = unsafe { ffi::CFStringCreateWithCString(ptr::null_mut(), text.as_ptr(), STRING_UTF8) };
    if cf.is_null() {
        Err(SecError::Bookmark("CFStringCreateWithCString".into()))
    } else {
        Ok(cf)
    }
}

fn cf_error_text(err: *mut c_void) -> String {
    if err.is_null() {
        return "unknown CoreFoundation error".into();
    }
    // SAFETY: `err` is a CFError from the bookmark APIs.
    let desc = unsafe { ffi::CFErrorCopyDescription(err) };
    // SAFETY: `err` is a +1 CFError from the bookmark APIs.
    unsafe { cf_release(err) };
    if desc.is_null() {
        return "CFError with no description".into();
    }
    let text = cf_string_text(desc);
    // SAFETY: `desc` is the +1 copy from CFErrorCopyDescription.
    unsafe { cf_release(desc) };
    text
}

fn cf_string_text(s: *mut c_void) -> String {
    // SAFETY: `s` is a live CFString.
    let ptr = unsafe { ffi::CFStringGetCStringPtr(s, STRING_UTF8) };
    if !ptr.is_null() {
        // SAFETY: non-null internal UTF-8 pointer owned by the CFString.
        return unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
    }
    let mut buf = [0i8; 512];
    // SAFETY: out-buffer is 512 bytes; encoding is UTF-8.
    let ok =
        unsafe { ffi::CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, STRING_UTF8) };
    if ok == 0 {
        return "unreadable CFString".into();
    }
    // SAFETY: CFStringGetCString wrote a NUL-terminated UTF-8 string.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

fn osstatus_text(status: i32) -> String {
    format!("OSStatus {status}")
}

/// SAFETY: `obj` is null or a +1 CFType this module owns.
unsafe fn cf_release(obj: *mut c_void) {
    if !obj.is_null() {
        // SAFETY: `obj` is a +1 CFType.
        unsafe { ffi::cf_release(obj) };
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_requirement, SecError};

    #[test]
    fn identifier_requirement_parses() {
        parse_requirement(r#"identifier "ternvale""#).expect("parse");
    }

    #[test]
    fn garbage_requirement_is_rejected() {
        let err = parse_requirement("not a requirement at all!!!").expect_err("bad");
        assert!(matches!(err, SecError::BadRequirement(_)));
    }
}
