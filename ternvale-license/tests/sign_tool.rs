//! Drive `ternvale-license-sign` (seed stays in a temp dir, never committed).

use std::path::Path;
use std::process::Command;

use ed25519_dalek::SigningKey;
use ternvale_license::blob;
use ternvale_license::check::{evaluate, require_new_session, EvaluateInput};
use ternvale_license::status::{Entitlement, GRACE_DAYS};
use ternvale_license::trial::MemoryStore;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ternvale-license-sign")
}

fn run(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(bin()).args(args).output().expect("sign tool");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn vk_from_seed(path: &Path) -> ed25519_dalek::VerifyingKey {
    let raw = std::fs::read(path).expect("seed");
    assert_eq!(raw.len(), 32, "seed must be 32 bytes");
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&raw);
    SigningKey::from_bytes(&seed).verifying_key()
}

#[test]
fn sign_tool_issues_valid_and_expired_licenses() {
    let dir = std::env::temp_dir().join(format!("tvlk-sign-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let seed = dir.join("license.seed");
    let valid = dir.join("valid.tvlk");
    let expired = dir.join("expired.tvlk");

    let (ok, _, err) = run(&["--gen-seed", seed.to_str().expect("utf8")]);
    assert!(ok, "gen-seed: {err}");
    assert_eq!(std::fs::metadata(&seed).expect("seed meta").len(), 32);

    let (ok, pub_hex, err) = run(&["--key", seed.to_str().expect("utf8"), "--print-public"]);
    assert!(ok, "print-public: {err}");
    let pub_hex = pub_hex.trim();
    assert_eq!(pub_hex.len(), 64);
    assert!(!err.to_lowercase().contains("seed bytes"));

    let vk = vk_from_seed(&seed);
    let expected: String = vk.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(pub_hex, expected);

    let (ok, _, err) = run(&[
        "--key",
        seed.to_str().expect("utf8"),
        "--edition",
        "pro",
        "--seats",
        "2",
        "--days",
        "30",
        "--id",
        "LIC-QUALITY",
        "--out",
        valid.to_str().expect("utf8"),
    ]);
    assert!(ok, "sign valid: {err}");
    let bytes = std::fs::read(&valid).expect("read valid");
    let got = blob::verify(&bytes, &vk).expect("verify valid");
    assert_eq!(got.payload.id, "LIC-QUALITY");
    assert_eq!(got.payload.edition, "pro");
    assert_eq!(got.payload.seats, 2);

    let (ok, _, err) = run(&[
        "--key",
        seed.to_str().expect("utf8"),
        "--edition",
        "personal",
        "--seats",
        "1",
        "--expiry-unix",
        "1000",
        "--id",
        "LIC-OLD",
        "--out",
        expired.to_str().expect("utf8"),
    ]);
    assert!(ok, "sign expired: {err}");
    let old = std::fs::read(&expired).expect("read expired");
    let blob = blob::verify(&old, &vk).expect("sig still valid");
    let store = MemoryStore::default();
    let late = 1000 + GRACE_DAYS * 86_400;
    let e = evaluate(EvaluateInput {
        public: &vk,
        store: &store,
        license_bytes: Some(&old),
        now: late,
        activate_url: None,
    })
    .expect("eval");
    assert!(matches!(e, Entitlement::Degraded { .. }), "{e:?}");
    assert!(blob.payload.id == "LIC-OLD");
    let msg = require_new_session(&e).expect_err("start refused");
    assert!(
        msg.to_string().contains("open, export, and backup"),
        "{msg}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
