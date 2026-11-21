//! HTTPS client for `ternvale --remote`.

use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;

use rustls::{ClientConnection, StreamOwned};
use serde_json::Value;

use crate::auth::TokenSecret;
use crate::error::GatewayError;
use crate::tls::{client_config, server_name};

/// Origin (`https://127.0.0.1:8443`) plus bearer and CA.
pub struct GatewayClient {
    host: String,
    port: u16,
    token: Option<TokenSecret>,
    tls: Arc<rustls::ClientConfig>,
}

/// HTTP result from the gateway.
#[derive(Debug)]
pub struct GatewayResponse {
    /// Status code.
    pub status: u16,
    /// Body.
    pub body: Vec<u8>,
    /// Content-Type if present.
    pub content_type: String,
}

impl GatewayClient {
    /// `base` is `https://host:port`. Trust `ca_path`.
    #[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(base))]
    pub fn connect(
        base: &str,
        token: Option<TokenSecret>,
        ca_path: &Path,
        client_cert: Option<&Path>,
        client_key: Option<&Path>,
    ) -> Result<Self, GatewayError> {
        let url = base
            .strip_prefix("https://")
            .ok_or_else(|| GatewayError::Config("remote URL must start with https://".into()))?;
        let url = url.trim_end_matches('/');
        let (host, port) = match url.rsplit_once(':') {
            Some((host, port)) => {
                let port: u16 = port.parse().map_err(|_| {
                    GatewayError::Config(format!("invalid remote port in {base:?}"))
                })?;
                (
                    host.trim_matches('[').trim_end_matches(']').to_string(),
                    port,
                )
            }
            None => (url.to_string(), 443),
        };
        let tls = client_config(ca_path, client_cert, client_key)?;
        tracing::info!(
            target: "ternvale::gateway",
            host = %host,
            port,
            "remote gateway client ready"
        );
        Ok(Self {
            host,
            port,
            token,
            tls,
        })
    }

    /// JSON request; returns parsed JSON when the body is JSON.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gateway",
        skip_all,
        fields(method, path)
    )]
    pub fn json(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, GatewayError> {
        let payload = match body {
            Some(value) => serde_json::to_vec(value)?,
            None => Vec::new(),
        };
        let response = self.request(method, path, "application/json", &payload)?;
        if response.body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&response.body).map_err(GatewayError::from)
    }

    /// Raw HTTP request over TLS.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gateway",
        skip_all,
        fields(method, path)
    )]
    pub fn request(
        &self,
        method: &str,
        path: &str,
        content_type: &str,
        body: &[u8],
    ) -> Result<GatewayResponse, GatewayError> {
        let server = server_name(&self.host)?;
        let conn = ClientConnection::new(Arc::clone(&self.tls), server)
            .map_err(|error| GatewayError::Tls(error.to_string()))?;
        let tcp = TcpStream::connect((self.host.as_str(), self.port))
            .map_err(|source| GatewayError::io("connect gateway", source))?;
        let mut tls = StreamOwned::new(conn, tcp);
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: application/json, image/png\r\nConnection: close\r\nContent-Length: {len}\r\n",
            host = self.host,
            port = self.port,
            len = body.len()
        );
        if !body.is_empty() {
            head.push_str("Content-Type: ");
            head.push_str(content_type);
            head.push_str("\r\n");
        }
        if let Some(token) = &self.token {
            head.push_str("Authorization: Bearer ");
            head.push_str(token.expose());
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        tls.write_all(head.as_bytes())
            .map_err(|source| GatewayError::io("write gateway request", source))?;
        if !body.is_empty() {
            tls.write_all(body)
                .map_err(|source| GatewayError::io("write gateway body", source))?;
        }
        tls.flush()
            .map_err(|source| GatewayError::io("flush gateway request", source))?;
        parse_response(&mut tls)
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
fn parse_response<R: std::io::Read>(stream: &mut R) -> Result<GatewayResponse, GatewayError> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    std::io::BufRead::read_line(&mut reader, &mut line)
        .map_err(|source| GatewayError::io("read gateway status", source))?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| GatewayError::Http("invalid gateway status line".into()))?;
    let mut headers = Vec::new();
    loop {
        line.clear();
        std::io::BufRead::read_line(&mut reader, &mut line)
            .map_err(|source| GatewayError::io("read gateway header", source))?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((n, v)) = line.split_once(':') {
            headers.push((n.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let len = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if len > 8 * 1024 * 1024 {
        return Err(GatewayError::Http("gateway response too large".into()));
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        std::io::Read::read_exact(&mut reader, &mut body)
            .map_err(|source| GatewayError::io("read gateway body", source))?;
    }
    let content_type = headers
        .iter()
        .find(|(k, _)| k == "content-type")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    Ok(GatewayResponse {
        status,
        body,
        content_type,
    })
}
