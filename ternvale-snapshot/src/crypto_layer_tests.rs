//! Encrypted COW layers: marker is absent from raw files, present on a
//! plaintext control stack.

use std::path::Path;
use std::process::Command;

use ternvale_crypto::{CryptoContext, Secret};

use super::{DeltaLayer, ImageStack, StackManifest, DEFAULT_BLOCK_SIZE};

const MARKER: &[u8] = b"TERNVALE-PLAINTEXT-MARKER";
const MIB: u64 = 1024 * 1024;

fn temp_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "tv-snap-xts-{}-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
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

fn seed_zeros(path: &Path, bytes: u64) {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .expect("base");
    file.set_len(bytes).expect("len");
}

fn write_marker(stack: &mut ImageStack, offset: u64) {
    let mut buf = vec![0u8; 512];
    buf[..MARKER.len()].copy_from_slice(MARKER);
    stack.write_at(&buf, offset).expect("write");
    stack.flush().expect("flush");
}

#[test]
fn encrypted_delta_hides_marker_plaintext_control_does_not() {
    let dir = temp_dir("mark");
    std::fs::create_dir_all(&dir).expect("dir");
    let virtual_size = 2 * MIB;
    let offset = u64::from(DEFAULT_BLOCK_SIZE);
    let needle = "TERNVALE-PLAINTEXT-MARKER";

    let plain_base = dir.join("plain-base.img");
    let plain_delta = dir.join("plain.tdelta");
    let plain_stack = dir.join("plain.tstack");
    seed_zeros(&plain_base, virtual_size);
    DeltaLayer::create(&plain_delta, virtual_size).expect("plain delta");
    StackManifest {
        base: plain_base.clone(),
        deltas: vec![plain_delta.clone()],
    }
    .save(&plain_stack)
    .expect("plain manifest");
    let mut stack = ImageStack::open(&plain_stack, false).expect("plain open");
    write_marker(&mut stack, offset);
    assert!(
        grep_c(&plain_delta, needle) >= 1,
        "unencrypted control delta must contain the marker"
    );

    let enc_base = dir.join("enc-base.img");
    let enc_delta = dir.join("enc.tdelta");
    let enc_stack = dir.join("enc.tstack");
    seed_zeros(&enc_base, virtual_size);
    DeltaLayer::create(&enc_delta, virtual_size).expect("enc delta");
    StackManifest {
        base: enc_base.clone(),
        deltas: vec![enc_delta.clone()],
    }
    .save(&enc_stack)
    .expect("enc manifest");
    let ctx = CryptoContext::from_dek(Secret::new(vec![8u8; 32])).expect("ctx");
    let mut stack =
        ImageStack::open_crypto(&enc_stack, false, Some(ctx.xts().clone())).expect("enc open");
    write_marker(&mut stack, offset);
    assert_eq!(
        grep_c(&enc_delta, needle),
        0,
        "encrypted delta must not contain plaintext marker"
    );
    assert_eq!(grep_c(&enc_base, needle), 0);
    let mut out = vec![0u8; 512];
    stack.read_at(&mut out, offset).expect("read");
    assert_eq!(&out[..MARKER.len()], MARKER);

    let wrong = CryptoContext::from_dek(Secret::new(vec![9u8; 32])).expect("wrong");
    let before = std::fs::read(&enc_delta).expect("before");
    let spy = ImageStack::open_crypto(&enc_stack, true, Some(wrong.xts().clone())).expect("wrong");
    spy.read_at(&mut out, offset).expect("wrong read");
    assert_ne!(&out[..MARKER.len()], MARKER);
    let after = std::fs::read(&enc_delta).expect("after");
    assert_eq!(before, after, "wrong-key read must not rewrite the delta");

    let _ = std::fs::remove_dir_all(&dir);
}
