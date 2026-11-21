//! Minimal RGB PNG encoder (8-bit, no ancillary chunks).

use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::error::GatewayError;

const PNG_SIG: &[u8] = &[137, 80, 78, 71, 13, 10, 26, 10];

/// Encode packed RGB (`width * height * 3` bytes) as PNG.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::gateway",
    skip_all,
    fields(width, height)
)]
pub fn encode_rgb(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>, GatewayError> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(3))
        .ok_or_else(|| GatewayError::Http("framebuffer dimensions overflow".into()))?;
    if rgb.len() != expected {
        return Err(GatewayError::Http(format!(
            "framebuffer size {} does not match {width}x{height} RGB",
            rgb.len()
        )));
    }
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(GatewayError::Http("framebuffer size out of range".into()));
    }
    let mut filtered = Vec::with_capacity((width as usize * 3 + 1) * height as usize);
    for row in 0..height as usize {
        filtered.push(0);
        let start = row * width as usize * 3;
        filtered.extend_from_slice(&rgb[start..start + width as usize * 3]);
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&filtered)
        .map_err(|source| GatewayError::io("zlib png", source))?;
    let idat = encoder
        .finish()
        .map_err(|source| GatewayError::io("zlib png finish", source))?;

    let mut out = Vec::from(PNG_SIG);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"IDAT", &idat);
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

/// One black pixel PNG (valid file for tests when no framebuffer exists).
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn placeholder() -> Result<Vec<u8>, GatewayError> {
    encode_rgb(1, 1, &[0, 0, 0])
}

/// True when `bytes` start with the PNG signature.
#[tracing::instrument(level = "debug", target = "ternvale::gateway", skip_all)]
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(PNG_SIG)
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_data = Vec::with_capacity(4 + data.len());
    crc_data.extend_from_slice(kind);
    crc_data.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_data).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xffff_ffff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_is_png() {
        let bytes = placeholder().expect("png");
        assert!(is_png(&bytes));
        assert!(bytes.len() > 40);
    }
}
