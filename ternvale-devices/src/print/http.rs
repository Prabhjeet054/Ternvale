//! Bounded HTTP/1.1 request parser for the IPP proxy. Guest-controlled.

use super::ops::{is_allowed, op_name, parse_header};
use ternvale_agent_proto::valid_printer_id;

/// Largest HTTP header block (request line + headers).
pub const MAX_HEADERS: usize = 16 * 1024;
/// Largest IPP body (print job). Unbounded growth is refused.
pub const MAX_BODY: usize = 32 * 1024 * 1024;

/// Why a request was refused (logged; never a panic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refuse {
    /// HTTP status to return.
    pub status: u16,
    /// Short reason for logs.
    pub reason: &'static str,
}

/// An allowed POST to `/printers/<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowed {
    /// Printer path id.
    pub printer_id: String,
    /// IPP operation-id.
    pub op: u16,
    /// IPP request-id.
    pub request_id: u32,
    /// IPP version major.
    pub version_major: u8,
    /// IPP version minor.
    pub version_minor: u8,
    /// Raw IPP body (headers + attributes + document).
    pub body: Vec<u8>,
}

/// Inspect a complete HTTP request (headers + body).
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(len = raw.len()))]
pub fn inspect(raw: &[u8]) -> Result<Allowed, Refuse> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or(Refuse {
            status: 400,
            reason: "missing header terminator",
        })?;
    if split > MAX_HEADERS {
        tracing::warn!(target: "ternvale::print", split, "http headers too large");
        return Err(Refuse {
            status: 431,
            reason: "headers too large",
        });
    }
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| Refuse {
        status: 400,
        reason: "headers are not utf-8",
    })?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or(Refuse {
        status: 400,
        reason: "empty request line",
    })?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    // Admin URLs are refused even on GET (CUPS web UI) before the POST-only check.
    if path.to_ascii_lowercase().contains("/admin") {
        tracing::warn!(target: "ternvale::print", method, path, "blocked CUPS admin URL");
        return Err(Refuse {
            status: 403,
            reason: "cups admin url",
        });
    }
    if method != "POST" {
        tracing::warn!(target: "ternvale::print", method, path, "blocked non-POST (CONNECT / other)");
        return Err(Refuse {
            status: 405,
            reason: "only POST is allowed",
        });
    }
    let printer_id = printer_id_from_path(path)?;
    let mut content_length: Option<usize> = None;
    let mut content_type = String::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Refuse {
            status: 400,
            reason: "malformed header",
        })?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "content-length" => {
                content_length = Some(value.parse().map_err(|_| Refuse {
                    status: 400,
                    reason: "bad content-length",
                })?);
            }
            "content-type" => content_type = value.to_ascii_lowercase(),
            "transfer-encoding" if value.to_ascii_lowercase().contains("chunked") => {
                tracing::warn!(target: "ternvale::print", "blocked chunked transfer-encoding");
                return Err(Refuse {
                    status: 411,
                    reason: "chunked encoding is not allowed",
                });
            }
            _ => {}
        }
    }
    let len = content_length.ok_or(Refuse {
        status: 411,
        reason: "content-length required",
    })?;
    if len == 0 || len > MAX_BODY {
        tracing::warn!(target: "ternvale::print", len, "ipp body length refused");
        return Err(Refuse {
            status: 413,
            reason: "body length out of range",
        });
    }
    if !content_type.is_empty()
        && !content_type.contains("application/ipp")
        && !content_type.contains("application/octet-stream")
    {
        tracing::warn!(target: "ternvale::print", content_type = %content_type, "blocked content-type");
        return Err(Refuse {
            status: 415,
            reason: "content-type must be application/ipp",
        });
    }
    let body_start = split + 4;
    if raw.len() < body_start + len {
        return Err(Refuse {
            status: 400,
            reason: "truncated body",
        });
    }
    let body = raw[body_start..body_start + len].to_vec();
    let (version_major, version_minor, op, request_id) = parse_header(&body).ok_or(Refuse {
        status: 400,
        reason: "ipp header too short",
    })?;
    if !is_allowed(op) {
        tracing::warn!(
            target: "ternvale::print",
            op = format!("{:#06x}", op),
            name = op_name(op),
            path,
            "blocked IPP operation (admin, URI-fetch, or CUPS extension)"
        );
        return Err(Refuse {
            status: 403,
            reason: "ipp operation is not a print-job op",
        });
    }
    tracing::debug!(
        target: "ternvale::print",
        printer_id = %printer_id,
        op = op_name(op),
        request_id,
        version_major,
        version_minor,
        "accepted IPP request"
    );
    Ok(Allowed {
        printer_id,
        op,
        request_id,
        version_major,
        version_minor,
        body,
    })
}

fn printer_id_from_path(path: &str) -> Result<String, Refuse> {
    let path = path.split('?').next().unwrap_or(path);
    let lower = path.to_ascii_lowercase();
    if path.contains("://") || path.contains('\\') || path.contains("..") {
        tracing::warn!(target: "ternvale::print", path, "blocked absolute or traversal path");
        return Err(Refuse {
            status: 403,
            reason: "absolute or traversal path",
        });
    }
    if lower.contains("/admin") || lower.starts_with("/admin") {
        tracing::warn!(target: "ternvale::print", path, "blocked CUPS admin URL");
        return Err(Refuse {
            status: 403,
            reason: "cups admin url",
        });
    }
    // Guest must not steer the proxy at another host:port via the path.
    if path.matches(':').count() > 0 && !path.starts_with("/printers/") && path != "/ipp/print" {
        tracing::warn!(target: "ternvale::print", path, "blocked path with host/port");
        return Err(Refuse {
            status: 403,
            reason: "path must not name a host or port",
        });
    }
    if path == "/ipp/print" || path == "/ipp/print/" {
        return Ok("pdf".into());
    }
    let rest = path
        .strip_prefix("/printers/")
        .ok_or(Refuse {
            status: 404,
            reason: "path is not /printers/<id>",
        })?
        .trim_end_matches('/');
    if !valid_printer_id(rest) {
        tracing::warn!(target: "ternvale::print", path, "blocked bad printer id");
        return Err(Refuse {
            status: 400,
            reason: "bad printer id",
        });
    }
    Ok(rest.to_string())
}

/// HTTP error response (no IPP body).
#[tracing::instrument(level = "trace", target = "ternvale::print", skip_all)]
pub fn http_error(status: u16, reason: &str) -> Vec<u8> {
    let text = match status {
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        _ => "Bad Request",
    };
    format!(
        "HTTP/1.1 {status} {text}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reason}",
        reason.len()
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::ops::OP_PRINT_JOB;

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
    fn accepts_print_job() {
        let allowed = inspect(&req("/printers/pdf", OP_PRINT_JOB, b"%PDF")).expect("ok");
        assert_eq!(allowed.printer_id, "pdf");
        assert_eq!(allowed.op, OP_PRINT_JOB);
    }

    #[test]
    fn blocks_admin_get_and_delete_printer() {
        let get = b"GET /admin HTTP/1.1\r\nHost: localhost:631\r\n\r\n";
        let err = inspect(get).expect_err("get");
        assert_eq!(err.status, 403);
        assert_eq!(err.reason, "cups admin url");
        let err = inspect(&req("/printers/pdf", 0x4004, b"")).expect_err("cups");
        assert_eq!(err.status, 403);
        assert_eq!(err.reason, "ipp operation is not a print-job op");
    }

    #[test]
    fn blocks_absolute_url_and_admin_path() {
        assert!(inspect(&req("http://127.0.0.1:9/printers/pdf", OP_PRINT_JOB, b"")).is_err());
        assert!(inspect(&req("/admin/conf", OP_PRINT_JOB, b"")).is_err());
    }
}
