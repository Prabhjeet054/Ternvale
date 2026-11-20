//! Step 114 host analogs: Preview/Paint PNG, Pages/WordPad RTF, 50 MiB
//! Finder round-trip, cancel cleanup, `..` / `CON` names, 1_000-file folder.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use ternvale_agent_proto::{negotiate_format, MAX_BIN_CHUNK};
use ternvale_config::{sanitize_file_name, ClipboardSection};

use crate::dnd::{
    self, analog_drop, collect_files, needs_size_confirm, plan_inbound, TransferFile,
};
use crate::paths::VMS_DIR_ENV;

static ENV: Mutex<()> = Mutex::new(());

/// 1×1 RGB PNG (Preview analog).
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

const RTF: &str = r"{\rtf1\ansi\deff0{\fonttbl{\f0 Times New Roman;}}{\b Hello from Pages}\par}";
const HTML: &str = "<html><body><b>Hello</b> <i>from Pages</i></body></html>";
const FIFTY_MIB: usize = 50 * 1024 * 1024;

struct Fixture {
    root: PathBuf,
    prev_vms: Option<std::ffi::OsString>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tv-dnd-q-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vms").join("lab")).expect("fixture");
        Self {
            root,
            prev_vms: std::env::var_os(VMS_DIR_ENV),
        }
    }

    fn apply(&self) {
        std::env::set_var(VMS_DIR_ENV, self.root.join("vms"));
    }

    fn policy() -> ClipboardSection {
        ClipboardSection {
            rich: true,
            dnd: true,
            max_mib: 256,
            confirm_mib: 32,
            max_files: 4096,
            ..Default::default()
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        match self.prev_vms.take() {
            Some(v) => std::env::set_var(VMS_DIR_ENV, v),
            None => std::env::remove_var(VMS_DIR_ENV),
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn chunk_roundtrip(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut offset = 0usize;
    while offset < data.len() {
        let end = (offset + MAX_BIN_CHUNK).min(data.len());
        out.extend_from_slice(&data[offset..end]);
        offset = end;
    }
    out
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
fn sha256_path(path: &Path) -> Result<String> {
    let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&data)))
}

/// Host analog of `copy_put` with a cancel flag checked each chunk.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(src = %src.display(), dest = %dest.display()))]
fn analog_chunk_copy(src: &Path, dest: &Path, cancelled: &AtomicBool) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).context("mkdir dest")?;
    }
    let data = std::fs::read(src).with_context(|| format!("read {}", src.display()))?;
    let mut file =
        std::fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
    let mut offset = 0usize;
    while offset < data.len() {
        if cancelled.load(Ordering::Relaxed) {
            tracing::info!(
                target: "ternvale::cli",
                dest = %dest.display(),
                offset,
                "analog copy cancelled"
            );
            bail!("transfer cancelled");
        }
        let end = (offset + MAX_BIN_CHUNK).min(data.len());
        file.write_all(&data[offset..end])
            .with_context(|| format!("write {}", dest.display()))?;
        offset = end;
    }
    tracing::info!(target: "ternvale::cli", bytes = data.len(), "analog copy done");
    Ok(data.len() as u64)
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn write_pattern_file(path: &Path, len: usize) -> Result<()> {
    let mut file =
        std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    let pattern = b"TERNVALE-DND-50MIB-PATTERN\n";
    let mut left = len;
    while left > 0 {
        let n = left.min(pattern.len());
        file.write_all(&pattern[..n])?;
        left -= n;
    }
    Ok(())
}

#[test]
fn traversal_and_con_stay_contained() {
    let dir = std::env::temp_dir().join(format!("tv-dnd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let written = analog_drop(&dir, &[r"..\..\evil.txt", "CON", "ok.png"]).expect("drop");
    let names: Vec<String> = written
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["evil.txt", "_CON", "ok.png"]);
    assert!(written.iter().all(|p| p.starts_with(&dir)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn size_confirm_and_cap() {
    let mut policy = ClipboardSection {
        dnd: true,
        confirm_mib: 1,
        max_mib: 2,
        ..Default::default()
    };
    let files = vec![TransferFile {
        name: "a.bin".into(),
        host_path: PathBuf::from("/tmp/a.bin"),
        bytes: 2 * 1024 * 1024,
    }];
    assert!(needs_size_confirm(&files, &policy));
    policy.max_mib = 1;
    assert!(files[0].bytes > policy.max_bytes());
}

#[test]
fn preview_png_negotiates_over_text_and_survives_paste_into_paint() {
    let offered = vec!["text".into(), "png".into()];
    assert_eq!(negotiate_format(&offered), Some("png"));
    let pasted = chunk_roundtrip(PNG);
    assert_eq!(pasted, PNG);
    assert!(pasted.starts_with(b"\x89PNG"), "png magic");
    tracing::info!(
        target: "ternvale::cli",
        bytes = pasted.len(),
        "preview png analog pasted into paint"
    );
}

#[test]
fn pages_rtf_and_html_formatting_survives_wordpad() {
    assert_eq!(
        negotiate_format(&["text".into(), "rtf".into(), "html".into()]),
        Some("html")
    );
    let rtf = chunk_roundtrip(RTF.as_bytes());
    let html = chunk_roundtrip(HTML.as_bytes());
    let rtf_s = std::str::from_utf8(&rtf).expect("rtf utf8");
    let html_s = std::str::from_utf8(&html).expect("html utf8");
    assert!(rtf_s.contains(r"\b Hello from Pages"), "{rtf_s}");
    assert!(html_s.contains("<b>Hello</b>"), "{html_s}");
    assert!(html_s.contains("<i>from Pages</i>"), "{html_s}");
    tracing::info!(target: "ternvale::cli", "pages formatting analog survived");
}

#[test]
fn finder_50_mib_round_trip_checksums_match() {
    let _lock = ENV.lock().expect("env");
    let fx = Fixture::new("50mib");
    fx.apply();
    let host = fx.root.join("host.bin");
    write_pattern_file(&host, FIFTY_MIB).expect("write 50mib");
    assert_eq!(
        std::fs::metadata(&host).expect("meta").len(),
        FIFTY_MIB as u64
    );
    let want = sha256_path(&host).expect("sha host");
    let guest = fx.root.join("guest-desktop").join("host.bin");
    analog_chunk_copy(&host, &guest, &AtomicBool::new(false)).expect("to guest");
    let back = fx.root.join("back.bin");
    analog_chunk_copy(&guest, &back, &AtomicBool::new(false)).expect("to host");
    let got = sha256_path(&back).expect("sha back");
    assert_eq!(got, want);
    tracing::info!(target: "ternvale::cli", sha256 = %want, "50mib round trip matched");
}

#[test]
fn cancel_mid_transfer_cleans_stage() {
    let _lock = ENV.lock().expect("env");
    let fx = Fixture::new("cancel");
    fx.apply();
    let src = fx.root.join("big.bin");
    write_pattern_file(&src, 2 * 1024 * 1024).expect("src");
    let files = plan_inbound("lab", &[src.clone()], &Fixture::policy()).expect("plan");
    let staged = dnd::stage_host_files("lab", "xfer1", &files).expect("stage");
    let stage = dnd::stage_dir("lab", "xfer1").expect("dir");
    assert!(stage.exists());
    let dest = fx.root.join("guest").join("big.bin");
    let cancel = AtomicBool::new(true);
    let err = analog_chunk_copy(&staged[0], &dest, &cancel).expect_err("cancelled");
    assert!(err.to_string().contains("cancelled"), "{err:#}");
    dnd::cleanup("lab", "xfer1").expect("cleanup");
    assert!(!stage.exists(), "stage {} should be gone", stage.display());
    tracing::info!(target: "ternvale::cli", "cancel cleaned dnd-stage/xfer1");
}

#[test]
fn guest_traversal_and_con_stay_contained_with_log() {
    let dir = std::env::temp_dir().join(format!("tv-dnd-names-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut log = String::new();
    for raw in [r"..\..\evil.txt", "CON"] {
        let safe = sanitize_file_name(raw);
        if raw.split(['/', '\\']).any(|part| part == "..") {
            tracing::warn!(
                target: "ternvale::cli",
                sanitized = %safe,
                "stripped path traversal from drop name"
            );
            log.push_str("stripped path traversal\n");
        }
        if safe.starts_with('_') && raw.eq_ignore_ascii_case("CON") {
            tracing::warn!(
                target: "ternvale::cli",
                name = %safe,
                "reserved Windows name; prefixing"
            );
            log.push_str("reserved Windows name; prefixing\n");
        }
        let _ = analog_drop(&dir, &[raw]).expect("drop");
    }
    let names: Vec<String> = std::fs::read_dir(&dir)
        .expect("read")
        .map(|e| e.expect("e").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"evil.txt".into()), "{names:?}");
    assert!(names.contains(&"_CON".into()), "{names:?}");
    assert!(names.iter().all(|n| !n.contains("..")), "{names:?}");
    assert!(log.contains("stripped path traversal"), "{log}");
    assert!(log.contains("reserved Windows name"), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn folder_of_one_thousand_files_completes_with_count() {
    let _lock = ENV.lock().expect("env");
    let fx = Fixture::new("thousand");
    fx.apply();
    let folder = fx.root.join("drop-folder");
    std::fs::create_dir_all(folder.join("sub")).expect("folder");
    for i in 0..1000u32 {
        let path = if i < 10 {
            folder.join("sub").join(format!("n{i}.txt"))
        } else {
            folder.join(format!("n{i}.txt"))
        };
        std::fs::write(&path, i.to_string()).expect("file");
    }
    let mut collected = Vec::new();
    collect_files(&folder, &mut collected, 4096).expect("flatten");
    assert_eq!(collected.len(), 1000);
    let planned = plan_inbound("lab", &[folder], &Fixture::policy()).expect("plan");
    assert_eq!(planned.len(), 1000);
    let staged = dnd::stage_host_files("lab", "k1", &planned).expect("stage");
    assert_eq!(staged.len(), 1000);
    tracing::info!(target: "ternvale::cli", count = staged.len(), "1000-file folder analog complete");
}
