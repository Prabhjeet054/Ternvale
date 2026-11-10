//! Hex-encoded binary fields (ExecOutput / Copy chunks). `Debug` is length only.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Binary payload on the wire as lowercase hex.
#[derive(Clone, PartialEq, Eq)]
pub struct HexBytes(pub Vec<u8>);

impl std::fmt::Debug for HexBytes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<{} bytes>", self.0.len())
    }
}

impl Serialize for HexBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for HexBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        decode(&text)
            .map(HexBytes)
            .map_err(serde::de::Error::custom)
    }
}

/// Lowercase hex.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(n = bytes.len()))]
pub fn encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Decode lowercase or uppercase hex. Odd length and non-hex are errors.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(n = text.len()))]
pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("hex payload has odd length".into());
    }
    if text.len() / 2 > crate::MAX_BIN_CHUNK {
        return Err(format!(
            "hex payload longer than {} bytes",
            crate::MAX_BIN_CHUNK
        ));
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

fn nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("hex payload has a non-hex digit".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rejects_junk() {
        let raw = b"\x00\xffhello";
        assert_eq!(decode(&encode(raw)).expect("decode"), raw);
        assert!(decode("abc").is_err());
        assert!(decode("zz").is_err());
        let shown = format!("{:?}", HexBytes(raw.to_vec()));
        assert_eq!(shown, format!("<{} bytes>", raw.len()));
        assert!(!shown.contains("hello"));
    }
}
