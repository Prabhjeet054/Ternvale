//! Little-endian loads from guest-controlled FUSE bodies. Short buffers read as 0.

#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
pub fn u32_at(buf: &[u8], off: usize) -> u32 {
    if off + 4 > buf.len() {
        return 0;
    }
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&buf[off..off + 4]);
    u32::from_le_bytes(raw)
}

#[tracing::instrument(level = "trace", target = "ternvale::virtiofs", skip_all)]
pub fn u64_at(buf: &[u8], off: usize) -> u64 {
    if off + 8 > buf.len() {
        return 0;
    }
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&buf[off..off + 8]);
    u64::from_le_bytes(raw)
}
