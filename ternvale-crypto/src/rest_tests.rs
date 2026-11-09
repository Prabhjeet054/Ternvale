//! Step 82 checks: log redaction, GCM tamper, `grep -c` on envelopes.

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

use super::*;

const MARKER: &[u8] = b"TERNVALE-PLAINTEXT-MARKER";

struct Buf(Arc<Mutex<Vec<u8>>>);

impl<'a> MakeWriter<'a> for Buf {
    type Writer = Guard;

    fn make_writer(&'a self) -> Self::Writer {
        Guard(Arc::clone(&self.0))
    }
}

struct Guard(Arc<Mutex<Vec<u8>>>);

impl Write for Guard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buf").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn grep_c(path: &Path, needle: &str) -> u32 {
    let out = Command::new("grep")
        .args(["-a", "-c", "--", needle])
        .arg(path)
        .output()
        .expect("grep");
    if !out.status.success() {
        return 0;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

fn b64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = u32::from(chunk[0]);
        let b = u32::from(chunk.get(1).copied().unwrap_or(0));
        let c = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (a << 16) | (b << 8) | c;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[test]
fn logs_never_contain_key_hex_or_base64() {
    let dek = Secret::new(vec![0x5au8; 32]);
    let hex = dek.to_hex();
    let b64 = b64(dek.expose());
    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Buf(Arc::clone(&buf)))
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            target: "ternvale::crypto",
            secret = ?dek,
            ctx = ?CryptoContext::from_dek(dek.clone()).expect("ctx"),
            "data key accepted"
        );
        let wrap = Secret::new(vec![0x11u8; 32]);
        let blob = wrap_dek(&wrap, &dek).expect("wrap");
        let _ = unwrap_dek(&wrap, &blob).expect("unwrap");
    });
    let text = String::from_utf8_lossy(&buf.lock().expect("buf")).into_owned();
    assert!(
        text.contains("[redacted]"),
        "expected redacted Debug in logs: {text}"
    );
    assert!(
        !text.contains(&hex),
        "DEK hex leaked into logs: {hex} in {text}"
    );
    assert!(
        !text
            .to_ascii_uppercase()
            .contains(&hex.to_ascii_uppercase()),
        "DEK hex (any case) leaked"
    );
    assert!(!text.contains(&b64), "DEK base64 leaked into logs");
}

#[test]
fn gcm_file_hides_marker_and_rejects_tamper() {
    let dir = std::env::temp_dir().join(format!("tv-gcm-rest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("nvram.fd");
    let key = GcmKey::from_key(&Secret::new(vec![3u8; 32])).expect("gcm");
    write_gcm_file(&key, &path, MARKER, AAD_NVRAM).expect("write");
    assert_eq!(grep_c(&path, "TERNVALE-PLAINTEXT-MARKER"), 0);
    let raw = std::fs::read(&path).expect("raw");
    assert!(is_envelope(&raw));
    assert_eq!(
        read_gcm_file(Some(&key), &path, AAD_NVRAM).expect("open"),
        MARKER
    );
    let mut tampered = raw;
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0x01;
    std::fs::write(&path, &tampered).expect("tamper");
    let err = read_gcm_file(Some(&key), &path, AAD_NVRAM).expect_err("auth");
    assert!(err.is_auth(), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wrong_dek_is_auth_error_without_touching_wrap_bytes() {
    let wrap = Secret::new(vec![1u8; 32]);
    let dek = Secret::new(vec![2u8; 32]);
    let blob = wrap_dek(&wrap, &dek).expect("wrap");
    let before = blob.clone();
    let wrong = Secret::new(vec![9u8; 32]);
    assert!(verify_dek(&wrong, &blob).expect_err("wrong").is_auth());
    assert_eq!(blob, before, "verify_dek must not mutate the wrap blob");
}
