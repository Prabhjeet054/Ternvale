//! Optional online activation. Sends **no** VM data.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::LicenseError;

/// Body posted when `TERNVALE_LICENSE_ACTIVATE_URL` is set: license id only.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActivateRequest {
    /// From the signed payload (`id`).
    pub license_id: String,
}

/// POST `{ "license_id": "..." }` to `url`. Timeouts are short. Never send
/// VM names, paths, or guest state.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::license",
    skip_all,
    fields(url = %url)
)]
pub fn activate(url: &str, license_id: &str) -> Result<(), LicenseError> {
    if license_id.is_empty() {
        return Err(LicenseError::Verify("empty license id".into()));
    }
    let body = serde_json::to_vec(&ActivateRequest {
        license_id: license_id.to_string(),
    })?;
    tracing::info!(
        target: "ternvale::license",
        bytes = body.len(),
        "activation POST (license_id only; no VM data)"
    );
    post_json(url, &body)
}

#[tracing::instrument(level = "debug", target = "ternvale::license", skip_all)]
fn post_json(url: &str, body: &[u8]) -> Result<(), LicenseError> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        LicenseError::Verify("activation URL must be http:// for this build".into())
    })?;
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = if path.is_empty() {
        "/"
    } else {
        &rest[hostport.len()..]
    };
    let addr = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:80")
    };
    let mut stream =
        TcpStream::connect(&addr).map_err(|e| LicenseError::io("activate connect", e))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| LicenseError::io("activate timeout", e))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| LicenseError::io("activate timeout", e))?;
    let host = hostport.split(':').next().unwrap_or(hostport);
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| LicenseError::io("activate write", e))?;
    stream
        .write_all(body)
        .map_err(|e| LicenseError::io("activate write", e))?;
    stream
        .flush()
        .map_err(|e| LicenseError::io("activate write", e))?;
    let mut buf = [0u8; 256];
    let n = stream
        .read(&mut buf)
        .map_err(|e| LicenseError::io("activate read", e))?;
    let head = String::from_utf8_lossy(&buf[..n]);
    if !head.contains("200") && !head.contains("204") {
        tracing::warn!(
            target: "ternvale::license",
            "activation endpoint returned a non-success status"
        );
        return Err(LicenseError::Verify("activation failed".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn posts_license_id_only() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let h = thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("acc");
            s.set_read_timeout(Some(Duration::from_secs(2))).expect("t");
            let mut req = Vec::new();
            let mut buf = [0u8; 256];
            loop {
                match s.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        req.extend_from_slice(&buf[..n]);
                        if req
                            .windows(b"\"license_id\":\"abc\"".len())
                            .any(|w| w == b"\"license_id\":\"abc\"")
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&req);
            assert!(text.contains("\"license_id\":\"abc\""), "{text:?}");
            assert!(!text.contains("vm_name"));
            s.write_all(
                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .expect("w");
        });
        activate(&format!("http://{addr}/activate"), "abc").expect("ok");
        h.join().expect("j");
    }
}
