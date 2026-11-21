//! Windows IPP Everywhere / inbox class driver printer (verify on ARM64).

use ternvale_agent_proto::{GuestPrinter, IPP_LOCAL_PORT};

use crate::AgentError;

/// PowerShell used to add an IPP printer.
///
/// TODO(verify): Windows 11 ARM64 still ships the inbox **Microsoft IPP Class
/// Driver** / IPP Everywhere (print class) so `Add-Printer -IppURL` does not
/// need a vendor driver. Confirm on the guest build used for live tests.
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all)]
pub fn add_printer_command(name: &str, id: &str) -> String {
    let uri = format!("ipp://127.0.0.1:{IPP_LOCAL_PORT}/printers/{id}");
    format!("Add-Printer -Name {name:?} -IppURL {uri:?} -ErrorAction Stop")
}

/// Apply or remove Ternvale printers.
#[cfg(windows)]
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(count = printers.len()))]
pub fn apply(printers: &[GuestPrinter]) -> Result<u32, AgentError> {
    crate::ipp_fwd::ensure_running();
    if printers.is_empty() {
        tracing::info!(target: "ternvale::print", "printer opt-in off; not adding Windows IPP printers");
        return Ok(0);
    }
    for printer in printers {
        let cmd = add_printer_command(&printer.name, &printer.id);
        match std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &cmd])
            .output()
        {
            Ok(out) if out.status.success() => {
                tracing::info!(
                    target: "ternvale::print",
                    id = %printer.id,
                    name = %printer.name,
                    "added Windows IPP printer"
                );
            }
            Ok(out) => {
                tracing::warn!(
                    target: "ternvale::print",
                    id = %printer.id,
                    status = %out.status,
                    "Add-Printer -IppURL failed"
                );
            }
            Err(source) => {
                return Err(AgentError::Action {
                    action: "Add-Printer",
                    source,
                });
            }
        }
    }
    Ok(printers.len() as u32)
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn apply(_printers: &[GuestPrinter]) -> Result<u32, AgentError> {
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipp_everywhere_command() {
        let cmd = add_printer_command("Print to PDF on Mac", "pdf");
        assert!(cmd.contains("Add-Printer"));
        assert!(cmd.contains("IppURL"));
        assert!(cmd.contains("ipp://127.0.0.1:8631/printers/pdf"));
    }
}
