//! "Print to PDF on Mac": extract a PDF from an IPP job and open Preview.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::ops::document_after_attributes;

/// How to open a PDF on the host (injectable; default is Preview).
pub type OpenPdf = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Default: `open -a Preview`. `TERNVALE_PREVIEW_CMD` overrides the binary for tests.
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(path = %path.display()))]
pub fn open_preview(path: &Path) -> Result<(), String> {
    let bin = std::env::var("TERNVALE_PREVIEW_CMD").unwrap_or_else(|_| "open".into());
    let mut cmd = std::process::Command::new(&bin);
    if bin == "open" {
        cmd.args(["-a", "Preview"]);
    }
    cmd.arg(path);
    match cmd.status() {
        Ok(status) if status.success() => {
            tracing::info!(
                target: "ternvale::print",
                path = %path.display(),
                "opened print job in Preview"
            );
            Ok(())
        }
        Ok(status) => Err(format!("preview exited {status}")),
        Err(error) => Err(format!("spawn preview: {error}")),
    }
}

/// Find `%PDF` in the IPP document and write it under `dir`.
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(dir = %dir.display(), body = body.len()))]
pub fn save_pdf(dir: &Path, body: &[u8]) -> Result<PathBuf, String> {
    let doc = document_after_attributes(body).unwrap_or(body);
    let start = doc.windows(5).position(|w| w == b"%PDF-").ok_or_else(|| {
        tracing::warn!(target: "ternvale::print", "print-to-pdf job had no %PDF magic");
        "document is not PDF".to_string()
    })?;
    let pdf = &doc[start..];
    std::fs::create_dir_all(dir).map_err(|e| format!("create pdf dir: {e}"))?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("ternvale-print-{n}.pdf"));
    std::fs::write(&path, pdf).map_err(|e| format!("write pdf: {e}"))?;
    tracing::info!(
        target: "ternvale::print",
        path = %path.display(),
        bytes = pdf.len(),
        "saved print-to-pdf job"
    );
    Ok(path)
}

/// Save then open. `open` may be a test spy.
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all)]
pub fn deliver_pdf(dir: &Path, body: &[u8], open: &OpenPdf) -> Result<PathBuf, String> {
    let path = save_pdf(dir, body)?;
    open(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn extracts_pdf_and_calls_open() {
        let dir = std::env::temp_dir().join(format!("tv-pdf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut body = vec![1, 1, 0, 2, 0, 0, 0, 1, 0x03];
        body.extend_from_slice(b"xx%PDF-1.4\n1 0 obj\n");
        let seen = Arc::new(Mutex::new(None));
        let flag = Arc::clone(&seen);
        let open: OpenPdf = Arc::new(move |p: &Path| {
            *flag.lock().expect("lock") = Some(p.to_path_buf());
            Ok(())
        });
        let path = deliver_pdf(&dir, &body, &open).expect("pdf");
        assert_eq!(seen.lock().expect("lock").clone().expect("opened"), path);
        let text = std::fs::read(&path).expect("read");
        assert!(text.starts_with(b"%PDF-1.4"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
