//! Rich clipboard formats, offer negotiation, and payload caps.

use crate::{MAX_BIN_CHUNK, MAX_COPY_PATH, MAX_NAME};

/// Formats the host and guest may offer (preference: png, dib, html, rtf, text, files).
pub const CLIP_FORMATS: [&str; 6] = ["png", "dib", "html", "rtf", "text", "files"];
/// Max formats in one offer.
pub const MAX_CLIP_FORMATS: usize = 8;
/// Max file names on a clipboard file list.
pub const MAX_CLIP_FILES: usize = 4096;
/// Largest assembled inline payload (PNG/RTF/HTML/DIB/text), in bytes.
pub const MAX_CLIP_INLINE: usize = 8 * 1024 * 1024;

/// True when `format` is a known clipboard type.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(format))]
pub fn is_clip_format(format: &str) -> bool {
    CLIP_FORMATS.contains(&format)
}

/// First mutually supported format, host preference order.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn negotiate_format(offered: &[String]) -> Option<&'static str> {
    for want in CLIP_FORMATS {
        if offered.iter().any(|got| got == want) {
            tracing::debug!(target: "ternvale::agent", format = want, "clipboard format chosen");
            return Some(want);
        }
    }
    tracing::warn!(target: "ternvale::agent", count = offered.len(), "no overlapping clipboard format");
    None
}

/// Range checks for `clipboard_offer`.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn validate_clipboard_offer(
    formats: &[String],
    sizes: &[u64],
    files: &[String],
) -> Result<(), String> {
    if formats.is_empty() || formats.len() > MAX_CLIP_FORMATS {
        return Err(format!(
            "clipboard_offer formats length must be 1..={MAX_CLIP_FORMATS}"
        ));
    }
    if !sizes.is_empty() && sizes.len() != formats.len() {
        return Err("clipboard_offer sizes must match formats".into());
    }
    for format in formats {
        if !is_clip_format(format) {
            return Err(format!("clipboard_offer unknown format {format}"));
        }
    }
    if files.len() > MAX_CLIP_FILES {
        return Err(format!(
            "clipboard_offer files longer than {MAX_CLIP_FILES} entries"
        ));
    }
    for name in files {
        if name.is_empty() || name.len() > MAX_COPY_PATH || name.contains('\0') {
            return Err("clipboard_offer file name empty, too long, or contains NUL".into());
        }
    }
    Ok(())
}

/// Range checks for one clipboard payload chunk.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn validate_clipboard_payload(format: &str, data_len: usize) -> Result<(), String> {
    if !is_clip_format(format) {
        return Err(format!("clipboard_payload unknown format {format}"));
    }
    if data_len > MAX_BIN_CHUNK {
        return Err(format!(
            "clipboard_payload longer than {MAX_BIN_CHUNK} bytes"
        ));
    }
    Ok(())
}

/// Range checks for `clipboard_take`.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn validate_clipboard_take(format: &str) -> Result<(), String> {
    if !is_clip_format(format) {
        Err(format!("clipboard_take unknown format {format}"))
    } else {
        Ok(())
    }
}

/// True when `echo` matches the last value we wrote (loop prevention).
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn is_echo_seq(last: Option<u64>, echo: u64) -> bool {
    last == Some(echo) && echo != 0
}

/// Reject an assembled payload that exceeds the inline cap.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(len))]
pub fn check_inline_len(len: usize) -> Result<(), String> {
    if len > MAX_CLIP_INLINE {
        Err(format!(
            "clipboard payload longer than {MAX_CLIP_INLINE} bytes"
        ))
    } else {
        Ok(())
    }
}

/// `clipboard_take` format must be a non-empty identifier.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn validate_reason(reason: &str) -> Result<(), String> {
    if reason.len() > MAX_NAME {
        Err(format!("reason longer than {MAX_NAME} bytes"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiate_prefers_png_over_text() {
        let offered = vec!["text".into(), "png".into(), "html".into()];
        assert_eq!(negotiate_format(&offered), Some("png"));
        assert_eq!(negotiate_format(&["nope".into()]), None);
    }

    #[test]
    fn echo_seq_ignores_own_offer() {
        assert!(is_echo_seq(Some(7), 7));
        assert!(!is_echo_seq(Some(7), 8));
        assert!(!is_echo_seq(None, 7));
        assert!(!is_echo_seq(Some(0), 0));
    }

    #[test]
    fn offer_rejects_unknown_format() {
        assert!(validate_clipboard_offer(&["gif".into()], &[], &[]).is_err());
        assert!(validate_clipboard_offer(&["text".into()], &[12], &[]).is_ok());
    }
}
