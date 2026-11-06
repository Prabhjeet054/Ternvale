//! Clipboard size cap and loop-prevention helpers (host and guest).

/// Largest `CF_UNICODETEXT` / JSON clipboard payload we accept, in UTF-8 bytes.
pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;

/// `Some` when `text` fits the cap.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(len = text.len()))]
pub fn capped(text: &str) -> Option<&str> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        tracing::warn!(
            target: "ternvale::agent",
            len = text.len(),
            max = MAX_CLIPBOARD_BYTES,
            "clipboard text exceeds cap; ignored"
        );
        None
    } else {
        Some(text)
    }
}

/// True when `incoming` is the text we just wrote (ignore our own set).
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn is_echo(last_set: Option<&str>, incoming: &str) -> bool {
    last_set == Some(incoming)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_rejects_oversized_text() {
        let big = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert!(capped(&big).is_none());
        assert_eq!(capped("ok"), Some("ok"));
    }

    #[test]
    fn echo_detection_ignores_our_own_set() {
        assert!(is_echo(Some("hello"), "hello"));
        assert!(!is_echo(Some("hello"), "world"));
        assert!(!is_echo(None, "hello"));
        let unicode = "hello 😀 வணக்கம் नमस्ते";
        assert!(is_echo(Some(unicode), unicode));
        assert!(!is_echo(Some(unicode), "hello 😀"));
    }
}
