//! Guest virtual printers advertised by the host (`printers_set`).

use serde::{Deserialize, Serialize};

/// vsock port the host IPP proxy listens on (`host-<port>.sock`).
pub const IPP_PORT: u32 = 8631;
/// Guest loopback TCP port the agent forwards to [`IPP_PORT`].
pub const IPP_LOCAL_PORT: u16 = 8631;
/// Path slug for "Print to PDF on Mac".
pub const PDF_PRINTER_ID: &str = "pdf";
/// Display name shown in the guest.
pub const PDF_PRINTER_NAME: &str = "Print to PDF on Mac";
/// Max printers in one `printers_set`.
pub const MAX_PRINTERS: usize = 32;
/// Longest printer id (URI path segment).
pub const MAX_PRINTER_ID: usize = 64;
/// Longest display name.
pub const MAX_PRINTER_NAME: usize = 128;

/// One virtual printer the guest should install (or an empty list to remove).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestPrinter {
    /// URI path id (`pdf`, sanitized CUPS name).
    pub id: String,
    /// Display name in the guest.
    pub name: String,
    /// `pdf` or `cups`.
    pub kind: String,
}

impl GuestPrinter {
    /// Range checks.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(id = %self.id))]
    pub fn validate(&self) -> Result<(), String> {
        if !valid_printer_id(&self.id) {
            return Err(format!(
                "printer id must be 1..={MAX_PRINTER_ID} [A-Za-z0-9_-]"
            ));
        }
        if self.name.is_empty() || self.name.len() > MAX_PRINTER_NAME {
            return Err(format!(
                "printer name length must be 1..={MAX_PRINTER_NAME}"
            ));
        }
        if self.name.contains('\0') {
            return Err("printer name must not contain NUL".into());
        }
        match self.kind.as_str() {
            "pdf" | "cups" => Ok(()),
            other => Err(format!("printer kind {other} is not pdf or cups")),
        }
    }
}

/// `printers_set.printers` list.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn validate_printers(printers: &[GuestPrinter]) -> Result<(), String> {
    if printers.len() > MAX_PRINTERS {
        return Err(format!("printers_set longer than {MAX_PRINTERS} entries"));
    }
    for (index, printer) in printers.iter().enumerate() {
        printer
            .validate()
            .map_err(|reason| format!("printers[{index}]: {reason}"))?;
        if printers[..index].iter().any(|other| other.id == printer.id) {
            return Err(format!("printers[{index}]: duplicate id"));
        }
    }
    Ok(())
}

/// Path-safe printer id.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
pub fn valid_printer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PRINTER_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Map a CUPS destination to a URI id.
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all)]
pub fn slug_printer_id(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if out.len() >= MAX_PRINTER_ID {
            break;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else if ch == ' ' {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("printer");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_and_validate() {
        assert_eq!(slug_printer_id("HP LaserJet"), "HP_LaserJet");
        let printer = GuestPrinter {
            id: PDF_PRINTER_ID.into(),
            name: PDF_PRINTER_NAME.into(),
            kind: "pdf".into(),
        };
        printer.validate().expect("pdf");
        assert!(GuestPrinter {
            id: "../x".into(),
            name: "x".into(),
            kind: "pdf".into(),
        }
        .validate()
        .is_err());
    }
}
