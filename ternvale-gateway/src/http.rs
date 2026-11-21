//! HTTP/1.1 request parsing with size limits. Chunked transfer is rejected.

use std::io::{BufRead, BufReader, Read, Write};

use crate::error::GatewayError;

/// Default max header block.
pub const MAX_HEADERS: usize = 8 * 1024;

/// One HTTP request after limits are applied.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// Uppercased method.
    pub method: String,
    /// Path + query (as sent).
    pub path: String,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    /// Body bytes (empty for GET).
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// First matching header (case-insensitive name already stored lowercased).
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(name))]
    pub fn header(&self, name: &str) -> Option<&str> {
        let want = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k == &want)
            .map(|(_, v)| v.as_str())
    }
}

/// Read one request from `reader`. `max_body` caps `Content-Length`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::gateway",
    skip_all,
    fields(max_body)
)]
pub fn read_request<R: Read>(
    reader: &mut BufReader<R>,
    max_body: usize,
) -> Result<HttpRequest, GatewayError> {
    let mut line = String::new();
    read_line(reader, &mut line, MAX_HEADERS)?;
    let mut parts = line.trim_end().split(' ');
    let method = parts
        .next()
        .ok_or_else(|| GatewayError::Http("empty request line".into()))?
        .to_ascii_uppercase();
    let path = parts
        .next()
        .ok_or_else(|| GatewayError::Http("request line missing path".into()))?
        .to_string();
    let version = parts
        .next()
        .ok_or_else(|| GatewayError::Http("request line missing version".into()))?;
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(GatewayError::Http(format!(
            "unsupported HTTP version {version}"
        )));
    }
    let mut headers = Vec::new();
    let mut header_bytes = line.len();
    loop {
        line.clear();
        read_line(reader, &mut line, MAX_HEADERS.saturating_sub(header_bytes))?;
        header_bytes = header_bytes.saturating_add(line.len());
        if header_bytes > MAX_HEADERS {
            return Err(GatewayError::Http("header block too large".into()));
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(GatewayError::Http("malformed header".into()));
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    if headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding"))
    {
        return Err(GatewayError::Http(
            "Transfer-Encoding is not supported".into(),
        ));
    }
    let content_length = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .map(|(_, v)| v.parse::<usize>())
        .transpose()
        .map_err(|_| GatewayError::Http("invalid Content-Length".into()))?
        .unwrap_or(0);
    if content_length > max_body {
        return Err(GatewayError::Http(format!(
            "request body {content_length} exceeds limit {max_body}"
        )));
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|source| GatewayError::io("read http body", source))?;
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

/// Write a complete HTTP/1.1 response and close.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::gateway",
    skip_all,
    fields(status)
)]
pub fn write_response<W: Write>(
    writer: &mut W,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, &str)],
) -> Result<(), GatewayError> {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    writer
        .write_all(head.as_bytes())
        .map_err(|source| GatewayError::io("write http headers", source))?;
    writer
        .write_all(body)
        .map_err(|source| GatewayError::io("write http body", source))?;
    writer
        .flush()
        .map_err(|source| GatewayError::io("flush http", source))?;
    Ok(())
}

/// JSON error object.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::gateway",
    skip_all,
    fields(status)
)]
pub fn write_json_error<W: Write>(
    writer: &mut W,
    status: u16,
    reason: &str,
    message: &str,
) -> Result<(), GatewayError> {
    let body = serde_json::json!({"ok": false, "error": message}).to_string();
    write_response(
        writer,
        status,
        reason,
        "application/json; charset=utf-8",
        body.as_bytes(),
        &[],
    )
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn read_line<R: Read>(
    reader: &mut BufReader<R>,
    line: &mut String,
    remaining: usize,
) -> Result<(), GatewayError> {
    let n = reader
        .read_line(line)
        .map_err(|source| GatewayError::io("read http line", source))?;
    if n == 0 {
        return Err(GatewayError::Http("unexpected eof".into()));
    }
    if line.len() > remaining {
        return Err(GatewayError::Http("request line too long".into()));
    }
    Ok(())
}

/// Split `/v1/vms/name/stop` into segments (empty first segment dropped).
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn path_segments(path: &str) -> Vec<&str> {
    let without_query = path.split('?').next().unwrap_or(path);
    without_query.split('/').filter(|s| !s.is_empty()).collect()
}

/// ASCII vm names (same rule as the CLI socket path).
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(name))]
pub fn valid_vm_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}
