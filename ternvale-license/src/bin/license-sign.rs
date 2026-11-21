//! Operator-only signer. Reads a 32-byte seed from a file or env; never logs it.
//!
//! Usage:
//!   ternvale-license-sign --edition pro --seats 1 --days 365 --id LIC-1 \
//!       --key /path/to/seed.bin --out license.tvlk
//!
//! `TERNVALE_LICENSE_SIGNING_KEY` may point at the seed file instead of `--key`.

use std::path::PathBuf;
use std::process::ExitCode;

use ed25519_dalek::SigningKey;
use ternvale_license::payload::{LicensePayload, PRODUCT};
use ternvale_license::secret::SigningSeed;
use ternvale_license::{blob, check};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut edition = String::from("personal");
    let mut seats: u32 = 1;
    let mut days: u64 = 365;
    let mut id = String::from("unnamed");
    let mut key_path: Option<PathBuf> =
        std::env::var_os("TERNVALE_LICENSE_SIGNING_KEY").map(PathBuf::from);
    let mut out = PathBuf::from("license.tvlk");
    let mut gen_seed: Option<PathBuf> = None;
    let mut print_public = false;
    let mut expiry_unix: Option<u64> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--gen-seed" => {
                gen_seed = Some(PathBuf::from(args.next().ok_or("--gen-seed needs a path")?))
            }
            "--print-public" => print_public = true,
            "--expiry-unix" => {
                expiry_unix = Some(
                    args.next()
                        .ok_or("--expiry-unix needs a value")?
                        .parse()
                        .map_err(|_| "expiry-unix")?,
                );
            }
            "--edition" => edition = args.next().ok_or("--edition needs a value")?,
            "--seats" => {
                seats = args
                    .next()
                    .ok_or("--seats needs a value")?
                    .parse()
                    .map_err(|_| "seats")?;
            }
            "--days" => {
                days = args
                    .next()
                    .ok_or("--days needs a value")?
                    .parse()
                    .map_err(|_| "days")?;
            }
            "--id" => id = args.next().ok_or("--id needs a value")?,
            "--key" => key_path = Some(PathBuf::from(args.next().ok_or("--key needs a path")?)),
            "--out" => out = PathBuf::from(args.next().ok_or("--out needs a path")?),
            "--help" | "-h" => {
                eprintln!(
                    "ternvale-license-sign --edition personal|pro|team --seats N --days N --id ID --key SEED --out FILE"
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if let Some(path) = gen_seed {
        let mut seed = SigningSeed([0u8; 32]);
        getrandom::getrandom(&mut seed.0).map_err(|_| "getrandom")?;
        std::fs::write(&path, seed.0).map_err(|e| format!("write seed: {e}"))?;
        eprintln!(
            "wrote 32-byte seed to {} (keep this file private)",
            path.display()
        );
        return Ok(());
    }
    let key_path = key_path.ok_or("need --key or TERNVALE_LICENSE_SIGNING_KEY")?;
    let seed = load_seed(&key_path)?;
    let signing = SigningKey::from_bytes(&seed.0);
    if print_public {
        let hex: String = signing
            .verifying_key()
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!("{hex}");
        return Ok(());
    }
    let now = check::unix_now();
    let payload = LicensePayload {
        product: PRODUCT.into(),
        edition,
        seats,
        expiry_unix: expiry_unix.unwrap_or_else(|| now.saturating_add(days.saturating_mul(86_400))),
        issued_unix: now,
        id,
    };
    let bytes = blob::sign(&payload, &signing).map_err(|e| e.to_string())?;
    std::fs::write(&out, bytes).map_err(|e| format!("write {}: {e}", out.display()))?;
    eprintln!(
        "wrote {} (id={} edition={} seats={} expiry_unix={})",
        out.display(),
        payload.id,
        payload.edition,
        payload.seats,
        payload.expiry_unix
    );
    Ok(())
}

fn load_seed(path: &std::path::Path) -> Result<SigningSeed, String> {
    let raw = std::fs::read(path).map_err(|e| format!("read key: {e}"))?;
    let bytes = if raw.len() == 32 {
        let mut a = [0u8; 32];
        a.copy_from_slice(&raw);
        a
    } else {
        let text = String::from_utf8(raw).map_err(|_| "key file is not 32 bytes or hex")?;
        let hex = text.trim();
        if hex.len() != 64 {
            return Err("key file must be 32 raw bytes or 64 hex chars".into());
        }
        let mut a = [0u8; 32];
        for (i, slot) in a.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| "key hex")?;
        }
        a
    };
    Ok(SigningSeed(bytes))
}
