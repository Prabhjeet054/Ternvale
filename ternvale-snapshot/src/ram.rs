//! zstd RAM payloads and packed dirty pages (magic + version).

use sha2::{Digest, Sha256};

use crate::SnapshotError;

/// Dirty-page blob magic (`TVDP`).
pub const DIRTY_MAGIC: [u8; 4] = *b"TVDP";
/// Packed dirty-page layout.
pub const DIRTY_VERSION: u32 = 1;

/// zstd-compress `data` (level 3).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(bytes = data.len()))]
pub fn compress(data: &[u8]) -> Result<Vec<u8>, SnapshotError> {
    zstd::encode_all(data, 3).map_err(|source| SnapshotError::InvalidRequest {
        detail: format!("zstd compress: {source}"),
    })
}

/// zstd-decompress; refuse output larger than `max`.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(bytes = data.len(), max))]
pub fn decompress(data: &[u8], max: usize) -> Result<Vec<u8>, SnapshotError> {
    use std::io::Read;
    let decoder =
        zstd::stream::Decoder::new(data).map_err(|source| SnapshotError::InvalidRequest {
            detail: format!("zstd decoder: {source}"),
        })?;
    let mut limited = decoder.take(max as u64 + 1);
    let mut out = Vec::new();
    limited
        .read_to_end(&mut out)
        .map_err(|source| SnapshotError::InvalidRequest {
            detail: format!("zstd decompress: {source}"),
        })?;
    if out.len() > max {
        return Err(SnapshotError::InvalidRequest {
            detail: "zstd output exceeds cap".into(),
        });
    }
    Ok(out)
}

/// Hex SHA-256 of `data`.
#[tracing::instrument(level = "trace", target = "ternvale::snapshot", skip_all)]
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Page size taken from the first dirty page.
#[must_use]
pub fn pack_dirty_page_hint(pages: &[(u64, Vec<u8>)]) -> u32 {
    pages.first().map(|(_, b)| b.len() as u32).unwrap_or(0)
}

/// Pack dirty pages: magic, version, page size, count, then (gpa, bytes)*.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(pages = pages.len()))]
pub fn pack_dirty(page_size: u32, pages: &[(u64, Vec<u8>)]) -> Result<Vec<u8>, SnapshotError> {
    let mut out = Vec::new();
    out.extend_from_slice(&DIRTY_MAGIC);
    out.extend_from_slice(&DIRTY_VERSION.to_le_bytes());
    out.extend_from_slice(&page_size.to_le_bytes());
    out.extend_from_slice(&(pages.len() as u32).to_le_bytes());
    for (gpa, bytes) in pages {
        if bytes.len() != page_size as usize {
            return Err(SnapshotError::InvalidRequest {
                detail: format!("dirty page at {gpa:#x} is {} bytes", bytes.len()),
            });
        }
        out.extend_from_slice(&gpa.to_le_bytes());
        out.extend_from_slice(bytes);
    }
    Ok(out)
}

/// Parse [`pack_dirty`].
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(len = raw.len()))]
pub fn unpack_dirty(raw: &[u8]) -> Result<Vec<(u64, Vec<u8>)>, SnapshotError> {
    if raw.len() < 16 {
        return Err(SnapshotError::Truncated {
            what: "dirty-page header",
        });
    }
    if raw[0..4] != DIRTY_MAGIC {
        return Err(SnapshotError::BadMagic {
            path: None,
            expected: "TVDP",
        });
    }
    let version = u32::from_le_bytes(raw[4..8].try_into().expect("ver"));
    if version != DIRTY_VERSION {
        return Err(SnapshotError::UnsupportedVersion {
            kind: "dirty-pages",
            found: version,
            supported: DIRTY_VERSION,
        });
    }
    let page_size = u32::from_le_bytes(raw[8..12].try_into().expect("ps")) as usize;
    let count = u32::from_le_bytes(raw[12..16].try_into().expect("n")) as usize;
    let mut at = 16;
    let mut pages = Vec::with_capacity(count);
    for _ in 0..count {
        if raw.len() < at + 8 + page_size {
            return Err(SnapshotError::Truncated {
                what: "dirty-page payload",
            });
        }
        let gpa = u64::from_le_bytes(raw[at..at + 8].try_into().expect("gpa"));
        at += 8;
        pages.push((gpa, raw[at..at + page_size].to_vec()));
        at += page_size;
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zstd_roundtrip_and_dirty_pack() {
        let raw = vec![0xABu8; 4096];
        let packed = compress(&raw).expect("zstd");
        assert!(packed.len() < raw.len());
        assert_eq!(decompress(&packed, raw.len() + 16).expect("dec"), raw);
        let pages = vec![(0x4000, vec![1u8; 16])];
        let blob = pack_dirty(16, &pages).expect("pack");
        assert_eq!(unpack_dirty(&blob).expect("unpack"), pages);
        let mut bad = blob.clone();
        bad[4..8].copy_from_slice(&99u32.to_le_bytes());
        let err = unpack_dirty(&bad).expect_err("ver");
        assert!(matches!(
            err,
            SnapshotError::UnsupportedVersion {
                kind: "dirty-pages",
                found: 99,
                ..
            }
        ));
    }
}
