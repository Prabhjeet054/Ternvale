//! One copy-on-write delta layer (`TVDS`).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::format::{
    bitmap_clear, bitmap_rank_before, bitmap_set, bitmap_test, DeltaHeader, DEFAULT_BLOCK_SIZE,
    DELTA_HEADER_LEN, DELTA_VERSION,
};
use crate::SnapshotError;

/// Open delta layer (read or write).
#[derive(Debug)]
pub struct DeltaLayer {
    path: PathBuf,
    file: File,
    header: DeltaHeader,
    bitmap: Vec<u8>,
    /// Unmapped/zero blocks (v2). Empty on v1 until upgraded.
    zero_bitmap: Vec<u8>,
    writable: bool,
}

impl DeltaLayer {
    /// Open an existing delta file.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(path = %path.display(), writable)
    )]
    pub fn open(path: &Path, writable: bool) -> Result<Self, SnapshotError> {
        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(path)
            .map_err(|source| SnapshotError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let mut header_buf = [0u8; DELTA_HEADER_LEN];
        file.read_exact_at(&mut header_buf, 0)
            .map_err(|source| SnapshotError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let header = match DeltaHeader::decode_at(&header_buf, Some(path)) {
            Ok(header) => header,
            Err(err) => {
                tracing::warn!(
                    target = "ternvale::snapshot",
                    path = %path.display(),
                    error = %err,
                    "delta layer open rejected"
                );
                return Err(err);
            }
        };
        let bitmap_len = header.bitmap_bytes as usize;
        let mut bitmap = vec![0u8; bitmap_len];
        if bitmap_len > 0 {
            file.read_exact_at(&mut bitmap, header.bitmap_offset)
                .map_err(|source| SnapshotError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        let mut zero_bitmap = vec![0u8; bitmap_len];
        if header.version >= DELTA_VERSION && bitmap_len > 0 {
            file.read_exact_at(&mut zero_bitmap, header.bitmap_offset + header.bitmap_bytes)
                .map_err(|source| SnapshotError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        tracing::debug!(
            target = "ternvale::snapshot",
            virtual_size = header.virtual_size,
            block_size = header.block_size,
            version = header.version,
            blocks = header.block_count(),
            "delta layer opened"
        );
        Ok(Self {
            path: path.to_path_buf(),
            file,
            header,
            bitmap,
            zero_bitmap,
            writable,
        })
    }

    /// Create a new empty delta atop a base of `virtual_size` bytes.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(path = %path.display(), virtual_size)
    )]
    pub fn create(path: &Path, virtual_size: u64) -> Result<Self, SnapshotError> {
        let header = DeltaHeader::empty_v2(virtual_size, DEFAULT_BLOCK_SIZE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|source| SnapshotError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let bitmap = vec![0u8; header.bitmap_bytes as usize];
        let zero_bitmap = vec![0u8; header.bitmap_bytes as usize];
        file.write_all_at(&header.encode(), 0)
            .map_err(|source| SnapshotError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if !bitmap.is_empty() {
            file.write_all_at(&bitmap, header.bitmap_offset)
                .map_err(|source| SnapshotError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
            file.write_all_at(&zero_bitmap, header.bitmap_offset + header.bitmap_bytes)
                .map_err(|source| SnapshotError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        file.set_len(header.data_offset)
            .map_err(|source| SnapshotError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        tracing::info!(
            target = "ternvale::snapshot",
            path = %path.display(),
            virtual_size = header.virtual_size,
            "created delta layer"
        );
        Ok(Self {
            path: path.to_path_buf(),
            file,
            header,
            bitmap,
            zero_bitmap,
            writable: true,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn virtual_size(&self) -> u64 {
        self.header.virtual_size
    }

    pub fn block_size(&self) -> u32 {
        self.header.block_size
    }

    pub fn has_block(&self, block_index: u64) -> bool {
        bitmap_test(&self.bitmap, block_index)
    }

    /// True when this layer maps the block as discarded zeros (not the base).
    pub fn has_zero(&self, block_index: u64) -> bool {
        bitmap_test(&self.zero_bitmap, block_index)
    }

    /// Read one full COW block from this layer.
    pub fn read_block(&self, block_index: u64, buf: &mut [u8]) -> Result<(), SnapshotError> {
        let bs = self.header.block_size as usize;
        if buf.len() < bs {
            return Err(SnapshotError::InvalidRequest {
                detail: "buffer shorter than block_size".into(),
            });
        }
        if !self.has_block(block_index) {
            return Err(SnapshotError::InvalidRequest {
                detail: format!("block {block_index} not in delta"),
            });
        }
        let rank = bitmap_rank_before(&self.bitmap, block_index);
        let offset = self.header.data_offset + rank * u64::from(self.header.block_size);
        self.file
            .read_exact_at(&mut buf[..bs], offset)
            .map_err(|source| SnapshotError::Io {
                path: self.path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Store a full COW block (overwrites if already present).
    pub fn write_block(&mut self, block_index: u64, data: &[u8]) -> Result<(), SnapshotError> {
        if !self.writable {
            return Err(SnapshotError::ReadOnly {
                path: self.path.clone(),
            });
        }
        let bs = self.header.block_size as usize;
        if data.len() < bs {
            return Err(SnapshotError::InvalidRequest {
                detail: "block data too short".into(),
            });
        }
        if block_index >= self.header.block_count() {
            return Err(SnapshotError::InvalidRequest {
                detail: format!("block {block_index} out of range"),
            });
        }
        bitmap_clear(&mut self.zero_bitmap, block_index);
        if self.has_block(block_index) {
            let rank = bitmap_rank_before(&self.bitmap, block_index);
            let offset = self.header.data_offset + rank * u64::from(self.header.block_size);
            self.file
                .write_all_at(&data[..bs], offset)
                .map_err(|source| SnapshotError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            self.persist_zero_bitmap()?;
            return Ok(());
        }
        bitmap_set(&mut self.bitmap, block_index);
        let offset = self
            .file
            .metadata()
            .map_err(|source| SnapshotError::Io {
                path: self.path.clone(),
                source,
            })?
            .len();
        self.file
            .write_all_at(&data[..bs], offset)
            .map_err(|source| SnapshotError::Io {
                path: self.path.clone(),
                source,
            })?;
        self.file
            .write_all_at(&self.bitmap, self.header.bitmap_offset)
            .map_err(|source| SnapshotError::Io {
                path: self.path.clone(),
                source,
            })?;
        self.persist_zero_bitmap()?;
        tracing::trace!(
            target = "ternvale::snapshot",
            block_index,
            file_offset = offset,
            "allocated delta block"
        );
        Ok(())
    }

    /// Mark `block_index` as discarded zeros (guest sees zeros, not the base).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(block_index)
    )]
    pub fn unmap_block(&mut self, block_index: u64) -> Result<(), SnapshotError> {
        if !self.writable {
            return Err(SnapshotError::ReadOnly {
                path: self.path.clone(),
            });
        }
        if block_index >= self.header.block_count() {
            return Err(SnapshotError::InvalidRequest {
                detail: format!("block {block_index} out of range"),
            });
        }
        if self.header.version < DELTA_VERSION {
            self.compact()?;
        }
        bitmap_set(&mut self.zero_bitmap, block_index);
        self.persist_zero_bitmap()?;
        tracing::debug!(
            target = "ternvale::snapshot",
            block_index,
            "delta block unmapped to zeros"
        );
        Ok(())
    }

    /// Grow the virtual size; new blocks start unmapped-from-base (holes).
    pub fn grow(&mut self, virtual_size: u64) -> Result<(), SnapshotError> {
        if !self.writable {
            return Err(SnapshotError::ReadOnly {
                path: self.path.clone(),
            });
        }
        let aligned = virtual_size - (virtual_size % u64::from(self.header.block_size));
        if aligned < self.header.virtual_size {
            return Err(SnapshotError::InvalidRequest {
                detail: "delta grow cannot shrink".into(),
            });
        }
        if aligned == self.header.virtual_size {
            return Ok(());
        }
        if self.header.version < DELTA_VERSION {
            self.compact()?;
        }
        self.header.virtual_size = aligned;
        let needed = crate::format::bitmap_bytes_for_blocks(self.header.block_count()) as usize;
        if needed > self.bitmap.len() {
            self.bitmap.resize(needed, 0);
            self.zero_bitmap.resize(needed, 0);
            self.header.bitmap_bytes = needed as u64;
            // Packed data stays where it is; bitmaps grew in place only if they
            // still fit before data_offset. Compact rewrites if they do not.
            let min_data = DELTA_HEADER_LEN as u64 + 2 * self.header.bitmap_bytes;
            if min_data > self.header.data_offset {
                self.compact()?;
            } else {
                self.file
                    .write_all_at(&self.header.encode(), 0)
                    .map_err(|source| SnapshotError::Io {
                        path: self.path.clone(),
                        source,
                    })?;
                self.file
                    .write_all_at(&self.bitmap, self.header.bitmap_offset)
                    .map_err(|source| SnapshotError::Io {
                        path: self.path.clone(),
                        source,
                    })?;
                self.persist_zero_bitmap()?;
            }
        } else {
            self.file
                .write_all_at(&self.header.encode(), 0)
                .map_err(|source| SnapshotError::Io {
                    path: self.path.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    /// Lower the virtual size (does not rewrite packed payloads).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(virtual_size)
    )]
    pub fn shrink(&mut self, virtual_size: u64) -> Result<(), SnapshotError> {
        if !self.writable {
            return Err(SnapshotError::ReadOnly {
                path: self.path.clone(),
            });
        }
        let aligned = virtual_size - (virtual_size % u64::from(self.header.block_size));
        if aligned > self.header.virtual_size {
            return self.grow(aligned);
        }
        self.header.virtual_size = aligned;
        self.file
            .write_all_at(&self.header.encode(), 0)
            .map_err(|source| SnapshotError::Io {
                path: self.path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Rewrite packed payloads, dropping unmapped blocks from the data region.
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(path = %self.path.display()))]
    pub fn compact(&mut self) -> Result<(u64, u64), SnapshotError> {
        if !self.writable {
            return Err(SnapshotError::ReadOnly {
                path: self.path.clone(),
            });
        }
        let before = allocated_bytes(&self.path)?;
        if self.header.version < DELTA_VERSION {
            self.header.version = DELTA_VERSION;
            self.zero_bitmap.resize(self.bitmap.len(), 0);
        }
        let tmp = self.path.with_extension("tdelta.compact");
        let out = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|source| SnapshotError::Io {
                path: tmp.clone(),
                source,
            })?;
        let mut new_bitmap = vec![0u8; self.bitmap.len()];
        let header = DeltaHeader::empty_v2(self.header.virtual_size, self.header.block_size);
        let mut data_at = header.data_offset;
        let bs = self.header.block_size as usize;
        let mut buf = vec![0u8; bs];
        for index in 0..self.header.block_count() {
            if self.has_zero(index) {
                bitmap_set(&mut self.zero_bitmap, index);
                continue;
            }
            if !self.has_block(index) {
                continue;
            }
            self.read_block(index, &mut buf)?;
            bitmap_set(&mut new_bitmap, index);
            out.write_all_at(&buf, data_at)
                .map_err(|source| SnapshotError::Io {
                    path: tmp.clone(),
                    source,
                })?;
            data_at += bs as u64;
        }
        out.write_all_at(&header.encode(), 0)
            .map_err(|source| SnapshotError::Io {
                path: tmp.clone(),
                source,
            })?;
        out.write_all_at(&new_bitmap, header.bitmap_offset)
            .map_err(|source| SnapshotError::Io {
                path: tmp.clone(),
                source,
            })?;
        out.write_all_at(
            &self.zero_bitmap,
            header.bitmap_offset + header.bitmap_bytes,
        )
        .map_err(|source| SnapshotError::Io {
            path: tmp.clone(),
            source,
        })?;
        out.sync_all().map_err(|source| SnapshotError::Io {
            path: tmp.clone(),
            source,
        })?;
        drop(out);
        std::fs::rename(&tmp, &self.path).map_err(|source| SnapshotError::Io {
            path: self.path.clone(),
            source,
        })?;
        *self = Self::open(&self.path, true)?;
        let after = allocated_bytes(&self.path)?;
        tracing::info!(
            target = "ternvale::snapshot",
            path = %self.path.display(),
            before,
            after,
            "delta compacted"
        );
        Ok((before, after))
    }

    fn persist_zero_bitmap(&self) -> Result<(), SnapshotError> {
        if self.header.version < DELTA_VERSION || self.zero_bitmap.is_empty() {
            return Ok(());
        }
        self.file
            .write_all_at(
                &self.zero_bitmap,
                self.header.bitmap_offset + self.header.bitmap_bytes,
            )
            .map_err(|source| SnapshotError::Io {
                path: self.path.clone(),
                source,
            })
    }

    pub fn flush(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

/// Host allocated bytes (`st_blocks` * 512 on Darwin).
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(path = %path.display()))]
pub fn allocated_bytes(path: &Path) -> Result<u64, SnapshotError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(meta.blocks() * 512)
}
