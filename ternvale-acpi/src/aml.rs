//! A minimal AML encoder: package lengths, name strings, and `Scope`.
//!
//! ACPI 6.5 §20 (ACPI Machine Language Specification). Only the terms the
//! DSDT needs today are here; devices, methods, and resources come later.
//!
//! - NameSeg (§20.2.2): four characters, the first `A-Z` or `_`, the rest
//!   `A-Z`, `0-9`, or `_`. Shorter ASL names are padded with `_`.
//! - NameString (§20.2.2): optional `\` (RootChar) or `^` (ParentPrefixChar)
//!   prefixes, then one segment, `DualNamePrefix` + two, or
//!   `MultiNamePrefix` + count + segments.
//! - PkgLength (§20.2.4): counts its own bytes plus everything after it in the
//!   package, but not the opcode before it.
//! - DefScope (§20.2.5.1): `ScopeOp PkgLength NameString TermList`.

use crate::AcpiError;

/// `ScopeOp`.
pub const SCOPE_OP: u8 = 0x10;
/// `RootChar` (`\`).
pub const ROOT_CHAR: u8 = 0x5c;
/// `ParentPrefixChar` (`^`).
pub const PARENT_PREFIX_CHAR: u8 = 0x5e;
/// `DualNamePrefix`.
pub const DUAL_NAME_PREFIX: u8 = 0x2e;
/// `MultiNamePrefix`.
pub const MULTI_NAME_PREFIX: u8 = 0x2f;
/// `NullName`.
pub const NULL_NAME: u8 = 0x00;
/// Largest value a PkgLength can hold (28 bits).
pub const MAX_PKG_LENGTH: usize = (1 << 28) - 1;

/// The PkgLength bytes for a package whose bytes after the PkgLength total
/// `content` bytes.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(content))]
pub fn pkg_length(content: usize) -> Result<Vec<u8>, AcpiError> {
    for follow in 0..=3usize {
        let Some(total) = content.checked_add(1 + follow) else {
            break;
        };
        let limit = if follow == 0 {
            0x3f
        } else {
            (1usize << (4 + 8 * follow)) - 1
        };
        if total > limit {
            continue;
        }
        if follow == 0 {
            return Ok(vec![total as u8]);
        }
        // Lead byte: bits 7-6 count the bytes that follow, bits 3-0 hold the
        // low nibble; each following byte holds the next 8 bits.
        let mut out = Vec::with_capacity(1 + follow);
        out.push(((follow as u8) << 6) | (total & 0x0f) as u8);
        for index in 0..follow {
            out.push((total >> (4 + 8 * index)) as u8);
        }
        return Ok(out);
    }
    tracing::warn!(target: "ternvale::acpi", content, "aml package too long for PkgLength");
    Err(AcpiError::PkgLengthTooLarge { len: content })
}

/// One NameSeg, padded with `_` to four bytes.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(seg = %seg))]
pub fn name_seg(seg: &str) -> Result<[u8; 4], AcpiError> {
    let bytes = seg.as_bytes();
    let lead_ok = |byte: u8| byte.is_ascii_uppercase() || byte == b'_';
    let valid = (1..=4).contains(&bytes.len())
        && lead_ok(bytes[0])
        && bytes
            .iter()
            .all(|byte| lead_ok(*byte) || byte.is_ascii_digit());
    if !valid {
        tracing::warn!(target: "ternvale::acpi", seg = %seg, "invalid aml name segment");
        return Err(AcpiError::BadNameSeg {
            name: seg.to_string(),
        });
    }
    let mut out = *b"____";
    out[..bytes.len()].copy_from_slice(bytes);
    Ok(out)
}

/// An ASL-style path such as `\_SB`, `^PCI0.S00`, or `DEV0` as a NameString.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(path = %path))]
pub fn name_string(path: &str) -> Result<Vec<u8>, AcpiError> {
    let bad = |reason: &'static str| {
        tracing::warn!(target: "ternvale::acpi", path = %path, reason, "invalid aml name path");
        AcpiError::BadNamePath {
            path: path.to_string(),
            reason,
        }
    };
    let mut out = Vec::new();
    let mut rest = path;
    if let Some(stripped) = rest.strip_prefix('\\') {
        out.push(ROOT_CHAR);
        rest = stripped;
    } else {
        while let Some(stripped) = rest.strip_prefix('^') {
            out.push(PARENT_PREFIX_CHAR);
            rest = stripped;
        }
    }
    if rest.is_empty() {
        if out.is_empty() {
            return Err(bad("empty path"));
        }
        out.push(NULL_NAME);
        return Ok(out);
    }
    let segs = rest
        .split('.')
        .map(name_seg)
        .collect::<Result<Vec<_>, _>>()?;
    match segs.len() {
        1 => {}
        2 => out.push(DUAL_NAME_PREFIX),
        count => {
            let count = u8::try_from(count).map_err(|_| bad("more than 255 segments"))?;
            out.push(MULTI_NAME_PREFIX);
            out.push(count);
        }
    }
    segs.iter().for_each(|seg| out.extend_from_slice(seg));
    Ok(out)
}

/// `Scope (path) { body }`, where `body` is already-encoded AML terms.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(path = %path, body = body.len())
)]
pub fn scope(path: &str, body: &[u8]) -> Result<Vec<u8>, AcpiError> {
    let name = name_string(path)?;
    let length = pkg_length(name.len() + body.len())?;
    let mut out = Vec::with_capacity(1 + length.len() + name.len() + body.len());
    out.push(SCOPE_OP);
    out.extend_from_slice(&length);
    out.extend_from_slice(&name);
    out.extend_from_slice(body);
    Ok(out)
}

#[cfg(test)]
#[path = "aml_tests.rs"]
mod tests;
