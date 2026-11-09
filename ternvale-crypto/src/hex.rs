//! Hex for Keychain and the control-socket data key. Never log the result.

use crate::CryptoError;

#[tracing::instrument(level = "trace", target = "ternvale::crypto", skip_all)]
pub fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[tracing::instrument(level = "trace", target = "ternvale::crypto", skip_all)]
pub fn decode(text: &str) -> Result<Vec<u8>, CryptoError> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return Err(CryptoError::BadHex);
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = nibble(bytes[i])?;
        let lo = nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn nibble(byte: u8) -> Result<u8, CryptoError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(CryptoError::BadHex),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        assert_eq!(encode(&[0xde, 0xad]), "dead");
        assert_eq!(decode("DeAd").expect("hex"), vec![0xde, 0xad]);
        assert!(decode("zzz").is_err());
        assert!(decode("abc").is_err());
    }
}
