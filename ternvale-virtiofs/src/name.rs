//! Guest path components. Every LOOKUP/CREATE name is checked here before
//! `openat`. Windows guests also get reserved-name / illegal-char / MAX_PATH
//! checks and case-insensitive lookup.

use std::os::unix::io::RawFd;
use std::path::Path;

use crate::sys;

/// Longest component we accept.
pub const MAX_NAME: usize = 255;
/// Win32 `MAX_PATH` including a `Z:` drive prefix (WinFsp default).
pub const WIN_MAX_PATH: usize = 260;

const WIN_ILLEGAL: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Reject empty names, `.`, `..`, embedded `/` or NUL, and overlong names.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(name))]
pub fn check(name: &str) -> Result<(), i32> {
    if name.is_empty() || name.len() > MAX_NAME {
        tracing::warn!(
            target: "ternvale::virtiofs",
            name,
            reason = "empty or longer than 255",
            "rejected share name"
        );
        return Err(libc::EINVAL);
    }
    if name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        tracing::warn!(
            target: "ternvale::virtiofs",
            name,
            reason = "dot, dot-dot, slash, or NUL",
            "rejected share name"
        );
        return Err(libc::EPERM);
    }
    Ok(())
}

/// Extra rules for Windows guests (reserved DOS names, illegal Win32
/// characters, trailing dots/spaces).
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(name))]
pub fn check_windows(name: &str) -> Result<(), i32> {
    check(name)?;
    if name.ends_with(' ') || name.ends_with('.') {
        tracing::warn!(
            target: "ternvale::virtiofs",
            name,
            reason = "Windows names cannot end in space or dot",
            "rejected share name"
        );
        return Err(libc::EINVAL);
    }
    if name.chars().any(|ch| ch < ' ' || WIN_ILLEGAL.contains(&ch)) {
        tracing::warn!(
            target: "ternvale::virtiofs",
            name,
            reason = "character is illegal on Windows",
            "rejected share name"
        );
        return Err(libc::EINVAL);
    }
    if is_reserved(name) {
        tracing::warn!(
            target: "ternvale::virtiofs",
            name,
            reason = "Windows reserved device name",
            "rejected share name"
        );
        return Err(libc::EINVAL);
    }
    Ok(())
}

/// Guest `Z:\` + relative host path must stay within [`WIN_MAX_PATH`].
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(name))]
pub fn check_windows_path(root: &Path, parent: &Path, name: &str) -> Result<(), i32> {
    let rel = parent.strip_prefix(root).unwrap_or(parent);
    let mut guest = String::from("Z:");
    for part in rel.iter() {
        guest.push('\\');
        guest.push_str(&part.to_string_lossy());
    }
    guest.push('\\');
    guest.push_str(name);
    if guest.len() > WIN_MAX_PATH {
        tracing::warn!(
            target: "ternvale::virtiofs",
            name,
            path_len = guest.len(),
            limit = WIN_MAX_PATH,
            reason = "Windows MAX_PATH",
            "rejected share name"
        );
        return Err(libc::ENAMETOOLONG);
    }
    Ok(())
}

/// POSIX checks, then Windows policy when `windows` is set. `creating` rejects
/// a case-insensitive collision with an existing sibling.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtiofs",
    skip_all,
    fields(name, windows, creating)
)]
pub fn prepare(dir: RawFd, name: &str, windows: bool, creating: bool) -> Result<String, i32> {
    if windows {
        check_windows(name)?;
    } else {
        check(name)?;
        return Ok(name.to_string());
    }
    if creating {
        reject_case_create(dir, name)?;
        return Ok(name.to_string());
    }
    fold_lookup(dir, name)
}

/// Unique case-insensitive sibling, or the exact name when it exists.
#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(name))]
pub fn fold_lookup(dir: RawFd, name: &str) -> Result<String, i32> {
    let names = sys::readdir_names(dir).map_err(|e| sys::errno(&e))?;
    let mut hits: Vec<String> = names
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| n.eq_ignore_ascii_case(name) && n != "." && n != "..")
        .collect();
    if hits.iter().any(|n| n == name) {
        return Ok(name.to_string());
    }
    hits.sort();
    hits.dedup();
    match hits.len() {
        0 => Ok(name.to_string()),
        1 => {
            tracing::debug!(
                target: "ternvale::virtiofs",
                requested = name,
                actual = %hits[0],
                "Windows case-insensitive lookup"
            );
            Ok(hits[0].clone())
        }
        _ => {
            tracing::warn!(
                target: "ternvale::virtiofs",
                name,
                matches = hits.len(),
                reason = "ambiguous case-insensitive name",
                "rejected share lookup"
            );
            Err(libc::EEXIST)
        }
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::virtiofs", skip_all, fields(name))]
fn reject_case_create(dir: RawFd, name: &str) -> Result<(), i32> {
    let names = sys::readdir_names(dir).map_err(|e| sys::errno(&e))?;
    for (existing, _) in names {
        if existing != name && existing.eq_ignore_ascii_case(name) {
            tracing::warn!(
                target: "ternvale::virtiofs",
                name,
                existing = %existing,
                reason = "case-insensitive collision (Windows)",
                "rejected share create"
            );
            return Err(libc::EEXIST);
        }
    }
    Ok(())
}

fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

#[cfg(test)]
mod tests {
    use super::{check, check_windows, is_reserved, WIN_MAX_PATH};
    use std::path::Path;

    #[test]
    fn rejects_escape_and_dot() {
        assert!(check("..").is_err());
        assert!(check(".").is_err());
        assert!(check("a/b").is_err());
        assert!(check("").is_err());
        assert!(check("ok.txt").is_ok());
    }

    #[test]
    fn windows_rejects_reserved_illegal_and_trailing() {
        assert!(check_windows("CON").is_err());
        assert!(check_windows("aux.txt").is_err());
        assert!(check_windows("nul").is_err());
        assert!(check_windows("foo:bar").is_err());
        assert!(check_windows("a<b").is_err());
        assert!(check_windows("ends ").is_err());
        assert!(check_windows("ends.").is_err());
        assert!(check_windows("ok.txt").is_ok());
        assert!(is_reserved("COM1"));
        assert!(!is_reserved("console.txt"));
    }

    #[test]
    fn windows_max_path_is_260() {
        let root = Path::new("/share");
        let parent = Path::new("/share");
        let long = "a".repeat(WIN_MAX_PATH);
        assert_eq!(
            super::check_windows_path(root, parent, &long),
            Err(libc::ENAMETOOLONG)
        );
        assert!(super::check_windows_path(root, parent, "short.txt").is_ok());
    }
}
