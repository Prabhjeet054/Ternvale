//! Host CUPS queue listing (`lpstat`) and a localhost-only IPP forward.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use super::http::Allowed;
use super::ops::op_name;

/// Parse `lpstat -p` / `lpstat -a` lines (`printer NAME is …`).
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(len = text.len()))]
pub fn parse_lpstat(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let rest = line.strip_prefix("printer ").unwrap_or("");
        if rest.is_empty() {
            continue;
        }
        let name = rest.split_whitespace().next().unwrap_or("");
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    tracing::debug!(target: "ternvale::print", count = names.len(), "parsed host CUPS queues");
    names
}

/// Run `lpstat -p`. Missing `lpstat` → empty list (WARN).
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all)]
pub fn list_host_queues() -> Vec<String> {
    match std::process::Command::new("lpstat").arg("-p").output() {
        Ok(out) if out.status.success() => parse_lpstat(&String::from_utf8_lossy(&out.stdout)),
        Ok(out) => {
            tracing::warn!(
                target: "ternvale::print",
                status = %out.status,
                "lpstat -p failed"
            );
            Vec::new()
        }
        Err(error) => {
            tracing::warn!(target: "ternvale::print", error = %error, "lpstat not available");
            Vec::new()
        }
    }
}

/// POST the IPP body to `127.0.0.1:631` only. Never uses the guest Host header.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::print",
    skip_all,
    fields(queue, op = op_name(req.op), addr = %addr)
)]
pub fn forward_to_cups(
    addr: std::net::SocketAddr,
    queue: &str,
    req: &Allowed,
) -> Result<Vec<u8>, String> {
    if !addr.ip().is_loopback() {
        tracing::error!(target: "ternvale::print", addr = %addr, "refusing non-loopback CUPS address");
        return Err("cups address must be loopback".into());
    }
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| format!("connect cups: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| format!("cups read timeout: {e}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| format!("cups write timeout: {e}"))?;
    let path = format!("/printers/{queue}");
    let http = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost:{}\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        addr.port(),
        req.body.len()
    );
    stream
        .write_all(http.as_bytes())
        .and_then(|_| stream.write_all(&req.body))
        .map_err(|e| format!("write cups: {e}"))?;
    let mut resp = Vec::new();
    stream
        .read_to_end(&mut resp)
        .map_err(|e| format!("read cups: {e}"))?;
    tracing::debug!(
        target: "ternvale::print",
        queue,
        bytes = resp.len(),
        "forwarded IPP job to host CUPS"
    );
    Ok(resp)
}

/// Minimal successful-ok IPP message (RFC 8011 §4.1.2) wrapped in HTTP.
#[tracing::instrument(level = "trace", target = "ternvale::print", skip_all)]
pub fn ipp_ok_http(version_major: u8, version_minor: u8, request_id: u32) -> Vec<u8> {
    let mut ipp = vec![version_major, version_minor, 0x00, 0x00];
    ipp.extend_from_slice(&request_id.to_be_bytes());
    ipp.push(0x03);
    http_ipp(&ipp)
}

/// client-error-forbidden IPP status 0x0403.
#[tracing::instrument(level = "trace", target = "ternvale::print", skip_all)]
pub fn ipp_forbidden_http(version_major: u8, version_minor: u8, request_id: u32) -> Vec<u8> {
    let mut ipp = vec![version_major, version_minor, 0x04, 0x03];
    ipp.extend_from_slice(&request_id.to_be_bytes());
    ipp.push(0x03);
    http_ipp(&ipp)
}

fn http_ipp(ipp: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        ipp.len()
    )
    .into_bytes();
    out.extend_from_slice(ipp);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lpstat_printer_lines() {
        let text = "printer Office is idle.  enabled since …\nprinter PDF is idle.\n";
        assert_eq!(parse_lpstat(text), ["Office", "PDF"]);
    }
}
