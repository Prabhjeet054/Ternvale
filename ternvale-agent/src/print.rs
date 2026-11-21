//! Install guest virtual printers from `printers_set`.

use std::path::PathBuf;

use ternvale_agent_proto::{GuestPrinter, IPP_LOCAL_PORT};

use crate::AgentError;

/// Where generated CUPS snippets land (`TERNVALE_PRINT_STATE` or `/etc/cups`).
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all)]
pub fn state_dir() -> PathBuf {
    std::env::var_os("TERNVALE_PRINT_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/cups"))
}

/// Apply the host's printer list. Empty list removes Ternvale printers.
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(count = printers.len()))]
pub fn apply(printers: &[GuestPrinter]) -> Result<u32, AgentError> {
    crate::ipp_fwd::ensure_running();
    #[cfg(windows)]
    {
        return crate::win_print::apply(printers);
    }
    #[cfg(not(windows))]
    {
        apply_linux(printers)
    }
}

#[cfg(not(windows))]
fn apply_linux(printers: &[GuestPrinter]) -> Result<u32, AgentError> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|source| AgentError::Action {
        action: "create print state dir",
        source,
    })?;
    let snippet = linux_cups_conf(printers);
    let path = dir.join("ternvale-printers.conf");
    std::fs::write(&path, snippet).map_err(|source| AgentError::Action {
        action: "write cups snippet",
        source,
    })?;
    tracing::info!(
        target: "ternvale::print",
        path = %path.display(),
        count = printers.len(),
        "wrote Linux CUPS printer snippet"
    );
    for printer in printers {
        let uri = format!("ipp://127.0.0.1:{IPP_LOCAL_PORT}/printers/{}", printer.id);
        // Best effort: missing lpadmin is fine in tests / distros without CUPS.
        match std::process::Command::new("lpadmin")
            .args(["-p", &printer.name, "-E", "-v", &uri, "-m", "everywhere"])
            .output()
        {
            Ok(out) if out.status.success() => {
                tracing::info!(
                    target: "ternvale::print",
                    id = %printer.id,
                    name = %printer.name,
                    "lpadmin added IPP Everywhere queue"
                );
            }
            Ok(out) => {
                tracing::warn!(
                    target: "ternvale::print",
                    id = %printer.id,
                    status = %out.status,
                    "lpadmin failed (CUPS may be absent)"
                );
            }
            Err(error) => {
                tracing::debug!(target: "ternvale::print", error = %error, "lpadmin not installed");
            }
        }
    }
    if printers.is_empty() {
        tracing::info!(target: "ternvale::print", "printer opt-in off; guest queues cleared");
    }
    Ok(printers.len() as u32)
}

/// Linux CUPS drop-in pointing at the vsock IPP proxy (localhost forwarder).
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all)]
pub fn linux_cups_conf(printers: &[GuestPrinter]) -> String {
    let mut out = String::from("# Ternvale guest printers (generated; do not edit)\n");
    if printers.is_empty() {
        out.push_str("# none (print sharing disabled)\n");
        return out;
    }
    for printer in printers {
        out.push_str(&format!(
            "<Printer {}>\n  UUID urn:uuid:ternvale-{}\n  DeviceURI ipp://127.0.0.1:{IPP_LOCAL_PORT}/printers/{}\n  Accepting Yes\n  Shared No\n</Printer>\n",
            printer.name.replace(['<', '>', '&'], "_"),
            printer.id,
            printer.id
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_agent_proto::{PDF_PRINTER_ID, PDF_PRINTER_NAME};

    #[test]
    fn cups_snippet_and_empty_uninstall() {
        let printers = [GuestPrinter {
            id: PDF_PRINTER_ID.into(),
            name: PDF_PRINTER_NAME.into(),
            kind: "pdf".into(),
        }];
        let text = linux_cups_conf(&printers);
        assert!(text.contains("DeviceURI ipp://127.0.0.1:8631/printers/pdf"));
        assert!(linux_cups_conf(&[]).contains("none"));
    }
}
