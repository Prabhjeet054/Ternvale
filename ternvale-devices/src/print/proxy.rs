//! Unix-socket IPP proxy: guest vsock → allowlisted ops → CUPS or Preview.

use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use super::cups::{forward_to_cups, ipp_forbidden_http, ipp_ok_http};
use super::http::{http_error, inspect, MAX_BODY, MAX_HEADERS};
use super::ops::{op_name, OP_PRINT_JOB, OP_SEND_DOCUMENT};
use super::pdf::deliver_pdf;
use super::policy::{PrintPolicy, PrinterTarget};

/// Host IPP proxy errors.
#[derive(Debug, thiserror::Error)]
pub enum PrintProxyError {
    /// Listen socket.
    #[error("print proxy socket {}: {source}", path.display())]
    Socket {
        /// Path.
        path: PathBuf,
        /// OS error.
        source: std::io::Error,
    },
    /// Thread spawn.
    #[error("spawn print proxy: {source}")]
    Spawn {
        /// OS error.
        source: std::io::Error,
    },
}

/// Listens on `host-<IPP_PORT>.sock` until dropped.
pub struct PrintProxy {
    stop: Arc<AtomicBool>,
    path: PathBuf,
    thread: Option<JoinHandle<()>>,
}

impl PrintProxy {
    /// Bind `path` (mode 0600) and accept guest connections.
    #[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(vm = %policy.vm, path = %path.display()))]
    pub fn start(path: &Path, policy: PrintPolicy) -> Result<Self, PrintProxyError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| PrintProxyError::Socket {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        clear_stale(path)?;
        let listener = UnixListener::bind(path).map_err(|source| PrintProxyError::Socket {
            path: path.to_path_buf(),
            source,
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| PrintProxyError::Socket {
                path: path.to_path_buf(),
                source,
            },
        )?;
        listener
            .set_nonblocking(true)
            .map_err(|source| PrintProxyError::Socket {
                path: path.to_path_buf(),
                source,
            })?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let policy = Arc::new(policy);
        let thread = std::thread::Builder::new()
            .name("print-proxy".into())
            .spawn(move || accept_loop(&listener, &policy, &flag))
            .map_err(|source| PrintProxyError::Spawn { source })?;
        tracing::info!(
            target: "ternvale::print",
            path = %path.display(),
            "IPP print proxy listening"
        );
        Ok(Self {
            stop,
            path: path.to_path_buf(),
            thread: Some(thread),
        })
    }

    /// Socket path.
    #[tracing::instrument(level = "trace", target = "ternvale::print", skip_all)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrintProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn clear_stale(path: &Path) -> Result<(), PrintProxyError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(PrintProxyError::Socket {
            path: path.to_path_buf(),
            source,
        }),
        Ok(meta) if meta.file_type().is_socket() => {
            std::fs::remove_file(path).map_err(|source| PrintProxyError::Socket {
                path: path.to_path_buf(),
                source,
            })
        }
        Ok(_) => Err(PrintProxyError::Socket {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "path exists and is not a socket",
            ),
        }),
    }
}

fn accept_loop(listener: &UnixListener, policy: &Arc<PrintPolicy>, stop: &AtomicBool) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let policy = Arc::clone(policy);
                let _ = std::thread::Builder::new()
                    .name("print-job".into())
                    .spawn(move || handle_conn(stream, &policy));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                tracing::warn!(target: "ternvale::print", error = %error, "print proxy accept failed");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

fn handle_conn(mut stream: UnixStream, policy: &PrintPolicy) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    match read_request(&mut stream) {
        Ok(raw) => {
            let reply = dispatch(&raw, policy);
            if let Err(error) = stream.write_all(&reply) {
                tracing::debug!(target: "ternvale::print", error = %error, "print proxy write failed");
            }
        }
        Err(error) => {
            tracing::warn!(target: "ternvale::print", error = %error, "print proxy read failed");
            let _ = stream.write_all(&http_error(400, "bad request"));
        }
    }
}

fn read_request(stream: &mut UnixStream) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_HEADERS + MAX_BODY + 64 {
            return Err("request too large".into());
        }
        if let Some(split) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            if let Some(len) = content_length(&buf[..split]) {
                if buf.len() >= split + 4 + len {
                    break;
                }
            } else if buf.len() > split + 4 {
                break;
            }
        }
        if n < tmp.len()
            && buf.windows(4).any(|w| w == b"\r\n\r\n")
            && content_length(&buf).is_none()
        {
            break;
        }
    }
    Ok(buf)
}

fn content_length(head: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(head).ok()?;
    for line in text.split("\r\n") {
        let (n, v) = line.split_once(':')?;
        if n.eq_ignore_ascii_case("content-length") {
            return v.trim().parse().ok();
        }
    }
    None
}

/// Classify and answer one HTTP+IPP request (also used by tests and the fuzzer).
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(vm = %policy.vm, bytes = raw.len()))]
pub fn dispatch(raw: &[u8], policy: &PrintPolicy) -> Vec<u8> {
    match inspect(raw) {
        Err(refuse) => http_error(refuse.status, refuse.reason),
        Ok(req) => match policy.resolve(&req.printer_id) {
            None => {
                tracing::warn!(
                    target: "ternvale::print",
                    printer_id = %req.printer_id,
                    "printer id is not shared with this VM"
                );
                http_error(404, "printer not shared")
            }
            Some(PrinterTarget::Pdf) => handle_pdf(&req, policy),
            Some(PrinterTarget::Cups(queue)) => {
                match forward_to_cups(policy.cups_addr, &queue, &req) {
                    Ok(resp) => resp,
                    Err(error) => {
                        tracing::warn!(target: "ternvale::print", error = %error, queue = %queue, "CUPS forward failed");
                        http_error(502, "cups forward failed")
                    }
                }
            }
        },
    }
}

fn handle_pdf(req: &super::http::Allowed, policy: &PrintPolicy) -> Vec<u8> {
    if req.op == OP_PRINT_JOB || req.op == OP_SEND_DOCUMENT {
        match deliver_pdf(&policy.pdf_dir, &req.body, &policy.open_pdf) {
            Ok(_) => ipp_ok_http(req.version_major, req.version_minor, req.request_id),
            Err(error) => {
                tracing::warn!(target: "ternvale::print", error = %error, "print-to-pdf failed");
                ipp_forbidden_http(req.version_major, req.version_minor, req.request_id)
            }
        }
    } else {
        tracing::debug!(
            target: "ternvale::print",
            op = op_name(req.op),
            "pdf printer status/validate op"
        );
        ipp_ok_http(req.version_major, req.version_minor, req.request_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::ops::{OP_GET_PRINTER_ATTRIBUTES, OP_PAUSE_PRINTER, OP_PRINT_JOB};
    use crate::print::policy::noop_open;
    use ternvale_config::PrintSection;

    fn policy() -> PrintPolicy {
        PrintPolicy::from_config(
            "lab",
            &PrintSection {
                enabled: true,
                pdf: true,
                share: Vec::new(),
            },
            &[],
            std::env::temp_dir().join(format!("tv-print-{}", std::process::id())),
            noop_open(),
        )
    }

    fn req(path: &str, op: u16, extra: &[u8]) -> Vec<u8> {
        let mut body = vec![1, 1, (op >> 8) as u8, op as u8, 0, 0, 0, 1, 0x03];
        body.extend_from_slice(extra);
        let mut out = format!(
            "POST {path} HTTP/1.1\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn pdf_print_job_and_admin_blocked() {
        let policy = policy();
        let ok = dispatch(
            &req("/printers/pdf", OP_PRINT_JOB, b"%PDF-1.4\n%EOF\n"),
            &policy,
        );
        assert!(std::str::from_utf8(&ok).unwrap_or("").contains("200"));
        let attr = dispatch(
            &req("/printers/pdf", OP_GET_PRINTER_ATTRIBUTES, b""),
            &policy,
        );
        assert!(std::str::from_utf8(&attr).unwrap_or("").contains("200"));
        let admin = dispatch(b"GET /admin HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n", &policy);
        assert!(std::str::from_utf8(&admin).unwrap_or("").contains("403"));
        let pause = dispatch(&req("/printers/pdf", OP_PAUSE_PRINTER, b""), &policy);
        assert!(std::str::from_utf8(&pause).unwrap_or("").contains("403"));
        let unknown = dispatch(&req("/printers/Office", OP_PRINT_JOB, b"%PDF"), &policy);
        assert!(std::str::from_utf8(&unknown).unwrap_or("").contains("404"));
    }
}
