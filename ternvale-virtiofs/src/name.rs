//! Guest path components. Every LOOKUP/CREATE name is checked here before
//! `openat`.

/// Longest component we accept.
pub const MAX_NAME: usize = 255;

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

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn rejects_escape_and_dot() {
        assert!(check("..").is_err());
        assert!(check(".").is_err());
        assert!(check("a/b").is_err());
        assert!(check("").is_err());
        assert!(check("ok.txt").is_ok());
    }
}
