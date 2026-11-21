//! Which guest printer ids this VM may reach.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use ternvale_agent_proto::{slug_printer_id, GuestPrinter, PDF_PRINTER_ID, PDF_PRINTER_NAME};

use super::pdf::OpenPdf;

/// Shared printers and CUPS target. The proxy never uses the guest Host header.
#[derive(Clone)]
pub struct PrintPolicy {
    /// VM name for logs.
    pub vm: String,
    /// Advertise and accept `/printers/pdf`.
    pub pdf: bool,
    /// `(id, cups_queue_name)` pairs from `[print].share`.
    pub cups: Vec<(String, String)>,
    /// Host CUPS (`127.0.0.1:631` only).
    pub cups_addr: SocketAddr,
    /// Directory for Print to PDF files.
    pub pdf_dir: PathBuf,
    /// Preview opener.
    pub open_pdf: OpenPdf,
}

impl PrintPolicy {
    /// From a `[print]` table. `share` names that are not in `host_queues` are dropped (WARN).
    #[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(vm))]
    pub fn from_config(
        vm: &str,
        section: &ternvale_config::PrintSection,
        host_queues: &[String],
        pdf_dir: PathBuf,
        open_pdf: OpenPdf,
    ) -> Self {
        let mut cups = Vec::new();
        if section.enabled {
            for name in &section.share {
                if !host_queues.iter().any(|q| q == name) {
                    tracing::warn!(
                        target: "ternvale::print",
                        vm,
                        printer = %name,
                        "shared printer is not a host CUPS queue; skipped"
                    );
                    continue;
                }
                let id = unique_id(&slug_printer_id(name), &cups);
                cups.push((id, name.clone()));
            }
        }
        Self {
            vm: vm.to_string(),
            pdf: section.enabled && section.pdf,
            cups,
            cups_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 631),
            pdf_dir,
            open_pdf,
        }
    }

    /// Guest install list (empty when printing is off).
    #[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(vm = %self.vm))]
    pub fn guest_printers(&self) -> Vec<GuestPrinter> {
        let mut out = Vec::new();
        if self.pdf {
            out.push(GuestPrinter {
                id: PDF_PRINTER_ID.into(),
                name: PDF_PRINTER_NAME.into(),
                kind: "pdf".into(),
            });
        }
        for (id, name) in &self.cups {
            out.push(GuestPrinter {
                id: id.clone(),
                name: name.clone(),
                kind: "cups".into(),
            });
        }
        out
    }

    /// Look up a path id.
    #[tracing::instrument(level = "trace", target = "ternvale::print", skip_all)]
    pub fn resolve(&self, id: &str) -> Option<PrinterTarget> {
        if self.pdf && id == PDF_PRINTER_ID {
            return Some(PrinterTarget::Pdf);
        }
        self.cups
            .iter()
            .find(|(slug, _)| slug == id)
            .map(|(_, name)| PrinterTarget::Cups(name.clone()))
    }
}

/// Where an allowed job goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrinterTarget {
    /// Preview PDF.
    Pdf,
    /// Host CUPS queue name.
    Cups(String),
}

fn unique_id(base: &str, existing: &[(String, String)]) -> String {
    if !existing.iter().any(|(id, _)| id == base) {
        return base.to_string();
    }
    for n in 2..100 {
        let candidate = format!("{base}_{n}");
        if !existing.iter().any(|(id, _)| id == &candidate) {
            return candidate;
        }
    }
    format!("{base}_x")
}

/// No-op Preview opener for tests.
#[tracing::instrument(level = "trace", target = "ternvale::print", skip_all)]
pub fn noop_open() -> OpenPdf {
    Arc::new(|_path| Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_config::PrintSection;

    #[test]
    fn skips_unknown_host_queues() {
        let section = PrintSection {
            enabled: true,
            pdf: true,
            share: vec!["Office".into(), "Missing".into()],
        };
        let policy = PrintPolicy::from_config(
            "lab",
            &section,
            &["Office".into()],
            PathBuf::from("/tmp"),
            noop_open(),
        );
        let printers = policy.guest_printers();
        assert_eq!(printers.len(), 2);
        assert_eq!(printers[0].id, "pdf");
        assert_eq!(printers[1].name, "Office");
        assert!(policy.resolve("Missing").is_none());
        assert!(matches!(
            policy.resolve("Office"),
            Some(PrinterTarget::Cups(_))
        ));
    }
}
