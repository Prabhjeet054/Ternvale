//! Host analog of a 10-minute license-parser fuzz (no libFuzzer required).

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::blob::{self, fuzz_verify, sign};
    use crate::payload::{LicensePayload, PRODUCT};
    use ed25519_dalek::SigningKey;

    fn valid_seed() -> Vec<u8> {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let payload = LicensePayload {
            product: PRODUCT.into(),
            edition: "pro".into(),
            seats: 1,
            expiry_unix: 9_999_999_999,
            issued_unix: 1,
            id: "fuzz-seed".into(),
        };
        sign(&payload, &sk).expect("sign")
    }

    #[test]
    fn fuzz_verify_does_not_panic_on_seeds() {
        fuzz_verify(&[]);
        fuzz_verify(&[0xff; 8]);
        let mut hdr = Vec::from(*blob::MAGIC);
        hdr.extend_from_slice(&blob::VERSION.to_le_bytes());
        hdr.extend_from_slice(b"{}");
        hdr.extend_from_slice(&[0u8; 64]);
        fuzz_verify(&hdr);
        fuzz_verify(&valid_seed());
        fuzz_verify(&vec![0u8; 64 * 1024]);
    }

    /// Ten-minute soak. Override with `TERNVALE_LICENSE_FUZZ_SECS` (CI analog: 3).
    #[test]
    #[ignore = "license-fuzz-10m"]
    fn fuzz_parser_ten_minutes() {
        let secs: u64 = std::env::var("TERNVALE_LICENSE_FUZZ_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600);
        tracing::info!(
            target: "ternvale::license",
            secs,
            "license parser soak starting"
        );
        let until = Instant::now() + Duration::from_secs(secs);
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut n = 0u64;
        let valid = valid_seed();
        while Instant::now() < until {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            match state % 5 {
                0 => fuzz_verify(&valid),
                1 => fuzz_verify(&[]),
                2 => {
                    let mut buf = valid.clone();
                    let i = (state as usize) % buf.len().max(1);
                    if let Some(b) = buf.get_mut(i) {
                        *b ^= (state >> 8) as u8;
                    }
                    fuzz_verify(&buf);
                }
                _ => {
                    let len = (state as usize % 2048) + 1;
                    let mut buf = vec![0u8; len];
                    for (j, byte) in buf.iter_mut().enumerate() {
                        *byte = state.to_le_bytes()[j % 8] ^ (j as u8);
                    }
                    fuzz_verify(&buf);
                }
            }
            n += 1;
        }
        tracing::info!(
            target: "ternvale::license",
            secs,
            iterations = n,
            "license parser soak finished without panics"
        );
        assert!(n > 0);
    }
}
