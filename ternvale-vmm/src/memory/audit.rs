//! Fails if a GuestMemory write helper copies into RAM without `mark_dirty`.

use super::{GuestMemory, HOST_PAGE_SIZE};

const SRC: &str = concat!(include_str!("mod.rs"), include_str!("track.rs"));

/// Public write helpers that must call `mark_dirty` (or `write_bytes`).
const WRITE_FNS: &[&str] = &["pub fn write_bytes", "pub fn host_ptr_mut"];

fn untracked_write_helpers(src: &str) -> Vec<String> {
    let mut bad = Vec::new();
    for sig in WRITE_FNS {
        let Some(rest) = src.split(sig).nth(1) else {
            bad.push(format!("missing `{sig}`"));
            continue;
        };
        let end = rest
            .find("\n    pub fn ")
            .or_else(|| rest.find("\n    guest_int!"))
            .unwrap_or(rest.len().min(2500));
        let body = &rest[..end];
        if !body.contains("mark_dirty") {
            bad.push(format!("{sig} bypasses dirty tracker"));
        }
    }
    if let Some(write_u) = src.split("pub fn $write").nth(1) {
        if !write_u.contains("write_bytes") {
            bad.push("integer writes bypass write_bytes".into());
        }
    }
    bad
}

#[test]
fn write_helpers_call_mark_dirty() {
    let bad = untracked_write_helpers(SRC);
    assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn scratch_untracked_write_path_is_caught() {
    // Deliberate scratch change analog: a write helper that copies into RAM
    // without `mark_dirty` must fail this checker (the real source still passes).
    let scratch = r#"
    pub fn write_bytes(&mut self, gpa: u64, src: &[u8]) -> Result<(), MemoryError> {
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), region.host.as_ptr(), src.len());
        }
        Ok(())
    }
    pub fn host_ptr_mut(&self, gpa: u64, len: usize) -> Result<*mut u8, MemoryError> {
        self.host_ptr(gpa, len)
    }
    pub fn $write(&mut self, gpa: u64, value: $ty) -> Result<(), MemoryError> {
        Ok(())
    }
"#;
    let caught = untracked_write_helpers(scratch);
    assert!(
        caught.iter().any(|s| s.contains("write_bytes")),
        "scratch write_bytes must be reported: {caught:?}"
    );
    assert!(
        caught.iter().any(|s| s.contains("host_ptr_mut")),
        "scratch host_ptr_mut must be reported: {caught:?}"
    );
    assert!(untracked_write_helpers(SRC).is_empty());
    tracing::info!(
        target: "ternvale::mem",
        caught = caught.len(),
        "dirty-tracking audit caught untracked scratch write path"
    );
}

#[test]
fn host_ptr_is_read_only_and_host_ptr_mut_tracks() {
    let read = SRC.split("pub fn host_ptr(").nth(1).expect("host_ptr");
    let read_body = &read[..read.find("pub fn ").unwrap_or(800)];
    assert!(
        !read_body.contains("mark_dirty"),
        "host_ptr is the read path and must not mark dirty"
    );
    let mut mem = GuestMemory::new().expect("mem");
    mem.add_region(0, HOST_PAGE_SIZE).expect("region");
    mem.set_dirty_tracking(true);
    mem.write_u8(0, 0x11).expect("write");
    let pages = mem.take_dirty_pages().expect("take");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].0, 0);
    assert_eq!(pages[0].1[0], 0x11);
    let _ = mem.host_ptr(0, 1).expect("read ptr");
    assert!(!mem.has_dirty_pages());
    let ptr = mem.host_ptr_mut(HOST_PAGE_SIZE / 2, 1).expect("write ptr");
    // SAFETY: host_ptr_mut validated one byte in the region.
    unsafe {
        *ptr = 0x22;
    }
    let pages = mem.take_dirty_pages().expect("take2");
    assert_eq!(pages.len(), 1);
}

#[test]
fn integer_writes_are_tracked() {
    let mut mem = GuestMemory::new().expect("mem");
    mem.add_region(0, HOST_PAGE_SIZE).expect("region");
    mem.set_dirty_tracking(true);
    mem.write_u16(8, 0x3344).expect("u16");
    assert!(mem.has_dirty_pages());
}
