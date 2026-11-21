//! RFC 6455 handshake and unmasked server text frames.

use std::io::{Read, Write};

use crate::error::GatewayError;
use crate::http::HttpRequest;

const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// True when the request is a WebSocket upgrade.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn is_upgrade(request: &HttpRequest) -> bool {
    let upgrade = request
        .header("upgrade")
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    let connection = request
        .header("connection")
        .map(|v| v.to_ascii_lowercase().contains("upgrade"))
        .unwrap_or(false);
    upgrade && connection && request.header("sec-websocket-key").is_some()
}

/// `Sec-WebSocket-Accept` for `key`.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn accept_key(key: &str) -> String {
    let mut data = Vec::with_capacity(key.len() + GUID.len());
    data.extend_from_slice(key.as_bytes());
    data.extend_from_slice(GUID);
    base64_encode(&sha1(&data))
}

/// 101 Switching Protocols.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn write_handshake<W: Write>(
    writer: &mut W,
    request: &HttpRequest,
) -> Result<(), GatewayError> {
    let key = request
        .header("sec-websocket-key")
        .ok_or_else(|| GatewayError::Http("missing Sec-WebSocket-Key".into()))?;
    let accept = accept_key(key);
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    writer
        .write_all(head.as_bytes())
        .map_err(|source| GatewayError::io("websocket handshake", source))?;
    writer
        .flush()
        .map_err(|source| GatewayError::io("websocket handshake flush", source))?;
    Ok(())
}

/// Send an unmasked text frame.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all, fields(len = payload.len()))]
pub fn write_text<W: Write>(writer: &mut W, payload: &str) -> Result<(), GatewayError> {
    write_frame(writer, 0x1, payload.as_bytes())
}

/// Send close.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn write_close<W: Write>(writer: &mut W) -> Result<(), GatewayError> {
    write_frame(writer, 0x8, &[])
}

/// Read one data frame; returns `None` on close. Control frames are skipped.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn read_data_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, GatewayError> {
    loop {
        let mut header = [0u8; 2];
        if let Err(error) = reader.read_exact(&mut header) {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                return Ok(None);
            }
            return Err(GatewayError::io("websocket header", error));
        }
        let opcode = header[0] & 0x0f;
        let masked = header[1] & 0x80 != 0;
        let mut len = u64::from(header[1] & 0x7f);
        if len == 126 {
            let mut ext = [0u8; 2];
            reader
                .read_exact(&mut ext)
                .map_err(|source| GatewayError::io("websocket len16", source))?;
            len = u64::from(u16::from_be_bytes(ext));
        } else if len == 127 {
            let mut ext = [0u8; 8];
            reader
                .read_exact(&mut ext)
                .map_err(|source| GatewayError::io("websocket len64", source))?;
            len = u64::from_be_bytes(ext);
        }
        if len > 64 * 1024 {
            return Err(GatewayError::Http("websocket frame too large".into()));
        }
        let mut mask = [0u8; 4];
        if masked {
            reader
                .read_exact(&mut mask)
                .map_err(|source| GatewayError::io("websocket mask", source))?;
        }
        let mut payload = vec![0u8; len as usize];
        if len > 0 {
            reader
                .read_exact(&mut payload)
                .map_err(|source| GatewayError::io("websocket payload", source))?;
        }
        if masked {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
        }
        match opcode {
            0x8 => return Ok(None),
            0x9 => {
                // ping → ignore (no auto-pong on this path)
                continue;
            }
            0x0..=0x2 => return Ok(Some(payload)),
            _ => continue,
        }
    }
}

fn write_frame<W: Write>(writer: &mut W, opcode: u8, payload: &[u8]) -> Result<(), GatewayError> {
    let mut header = vec![0x80 | opcode];
    let len = payload.len();
    if len <= 125 {
        header.push(len as u8);
    } else if len <= 65535 {
        header.push(126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }
    writer
        .write_all(&header)
        .map_err(|source| GatewayError::io("websocket frame header", source))?;
    writer
        .write_all(payload)
        .map_err(|source| GatewayError::io("websocket frame payload", source))?;
    writer
        .flush()
        .map_err(|source| GatewayError::io("websocket frame flush", source))?;
    Ok(())
}

fn sha1(data: &[u8]) -> [u8; 20] {
    // FIPS 180-1 SHA-1 (WebSocket handshake only; not used for tokens).
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = if i < 20 {
                ((b & c) | ((!b) & d), 0x5A82_7999)
            } else if i < 40 {
                (b ^ c ^ d, 0x6ED9_EBA1)
            } else if i < 60 {
                ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC)
            } else {
                (b ^ c ^ d, 0xCA62_C1D6)
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let b2 = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
        out.push(T[(b0 >> 2) as usize] as char);
        out.push(T[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if i + 1 < bytes.len() {
            out.push(T[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < bytes.len() {
            out.push(T[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6455_example_key() {
        // RFC 6455 §1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }
}
