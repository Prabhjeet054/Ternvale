//! One copy-on-write delta layer (`TVDS`).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::format::{
    bitmap_bytes_for_blocks, bitmap_rank_before, bitmap_set, bitmap_test, DeltaHeader,
    DEFAULT_BLOCK_SIZE, DELTA_HEADER_LEN,
};
use crate::SnapshotError;

/// Open delta layer (read or write).
#[derive(Debug)]
pub struct DeltaLayer {
    path: PathBuf,
    file: File,
    header: DeltaHeader,
    bitmap: Vec<u8>,
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
        tracing::debug!(
            target = "ternvale::snapshot",
            virtual_size = header.virtual_size,
            block_size = header.block_size,
            blocks = header.block_count(),
            "delta layer opened"
        );
        Ok(Self {
            path: path.to_path_buf(),
            file,
            header,
            bitmap,
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
        let block_size = DEFAULT_BLOCK_SIZE;
        let aligned = virtual_size - (virtual_size % u64::from(block_size));
        let header = DeltaHeader {
            virtual_size: aligned,
            block_size,
            bitmap_offset: DELTA_HEADER_LEN as u64,
            bitmap_bytes: 0,
            data_offset: 0,
        };
        let blocks = header.block_count();
        let bitmap_bytes = bitmap_bytes_for_blocks(blocks);
        let header = DeltaHeader {
            virtual_size: aligned,
            block_size,
            bitmap_offset: DELTA_HEADER_LEN as u64,
            bitmap_bytes,
            data_offset: DELTA_HEADER_LEN as u64 + bitmap_bytes,
        };
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
            writable: true,
        })
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
        if self.has_block(block_index) {
            let rank = bitmap_rank_before(&self.bitmap, block_index);
            let offset = self.header.data_offset + rank * u64::from(self.header.block_size);
            self.file
                .write_all_at(&data[..bs], offset)
                .map_err(|source| SnapshotError::Io {
                    path: self.path.clone(),
                    source,
                })?;
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
        tracing::trace!(
            target = "ternvale::snapshot",
            block_index,
            file_offset = offset,
            "allocated delta block"
        );
        Ok(())
    }

    pub fn flush(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}
