//! Layered image: base raw file + ordered deltas (newest wins on read).

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::delta::DeltaLayer;
use crate::manifest::StackManifest;
use crate::SnapshotError;

/// Copy-on-write disk opened from a `.tstack` manifest or built in memory for tests.
pub struct ImageStack {
    base: File,
    base_path: PathBuf,
    layers: Vec<DeltaLayer>,
    virtual_size: u64,
    block_size: u32,
    read_only: bool,
}

impl ImageStack {
    /// Open a stack manifest (`.tstack`).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(path = %manifest_path.display(), read_only)
    )]
    pub fn open(manifest_path: &Path, read_only: bool) -> Result<Self, SnapshotError> {
        let manifest = StackManifest::load(manifest_path)?;
        Self::open_manifest(&manifest, read_only)
    }

    /// Open from a parsed manifest.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(read_only)
    )]
    pub fn open_manifest(manifest: &StackManifest, read_only: bool) -> Result<Self, SnapshotError> {
        let base = OpenOptions::new()
            .read(true)
            .write(false)
            .open(&manifest.base)
            .map_err(|source| SnapshotError::Io {
                path: manifest.base.clone(),
                source,
            })?;
        let base_len = base
            .metadata()
            .map_err(|source| SnapshotError::Io {
                path: manifest.base.clone(),
                source,
            })?
            .len();
        let block_size = crate::format::DEFAULT_BLOCK_SIZE;
        let virtual_size = base_len - (base_len % u64::from(block_size));
        let mut layers = Vec::new();
        for (index, delta_path) in manifest.deltas.iter().enumerate() {
            let writable = !read_only && index + 1 == manifest.deltas.len();
            layers.push(DeltaLayer::open(delta_path, writable)?);
            let layer_size = layers.last().expect("layer").virtual_size();
            if layer_size != virtual_size {
                return Err(SnapshotError::InvalidManifest {
                    path: delta_path.clone(),
                    detail: format!("delta virtual_size {layer_size} != base {virtual_size}"),
                });
            }
        }
        tracing::info!(
            target = "ternvale::snapshot",
            base = %manifest.base.display(),
            layers = layers.len(),
            virtual_size,
            read_only,
            "image stack ready"
        );
        Ok(Self {
            base,
            base_path: manifest.base.clone(),
            layers,
            virtual_size,
            block_size,
            read_only,
        })
    }

    /// Build a stack from a base image path and delta paths (oldest first).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(read_only)
    )]
    pub fn from_paths(
        base: &Path,
        deltas: &[PathBuf],
        read_only: bool,
    ) -> Result<Self, SnapshotError> {
        let manifest = StackManifest {
            base: base.to_path_buf(),
            deltas: deltas.to_vec(),
        };
        Self::open_manifest(&manifest, read_only)
    }

    /// Logical size in bytes.
    #[must_use]
    pub fn virtual_size(&self) -> u64 {
        self.virtual_size
    }

    #[must_use]
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// Read bytes at `offset` through the layer stack (newest delta first).
    #[tracing::instrument(level = "trace", target = "ternvale::snapshot", skip_all, fields(offset, len = buf.len()))]
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        if offset.saturating_add(buf.len() as u64) > self.virtual_size {
            return Err(io::Error::new(ErrorKind::InvalidInput, "read past end"));
        }
        let mut done = 0usize;
        while done < buf.len() {
            let off = offset + done as u64;
            let chunk = (buf.len() - done).min(self.chunk_until_block_end(off));
            self.read_span(&mut buf[done..done + chunk], off)?;
            done += chunk;
        }
        Ok(())
    }

    /// Write bytes at `offset`; only the top-most writable delta is modified (COW).
    #[tracing::instrument(level = "trace", target = "ternvale::snapshot", skip_all, fields(offset, len = buf.len()))]
    pub fn write_at(&mut self, buf: &[u8], offset: u64) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "read-only stack",
            ));
        }
        if offset.saturating_add(buf.len() as u64) > self.virtual_size {
            return Err(io::Error::new(ErrorKind::InvalidInput, "write past end"));
        }
        let top = self.layers.len();
        if top == 0 {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "stack has no writable delta layer",
            ));
        }
        let layer_index = top - 1;
        let mut done = 0usize;
        while done < buf.len() {
            let off = offset + done as u64;
            let chunk = (buf.len() - done).min(self.chunk_until_block_end(off));
            self.write_span(layer_index, &buf[done..done + chunk], off)?;
            done += chunk;
        }
        Ok(())
    }

    pub fn flush(&self) -> io::Result<()> {
        for layer in &self.layers {
            layer.flush()?;
        }
        Ok(())
    }

    fn chunk_until_block_end(&self, offset: u64) -> usize {
        let bs = u64::from(self.block_size);
        let block_start = (offset / bs) * bs;
        let block_end = block_start + bs;
        (block_end - offset).min(bs) as usize
    }

    fn block_bytes(&self) -> usize {
        self.block_size as usize
    }

    fn read_span(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let bs = u64::from(self.block_size);
        let block_index = offset / bs;
        let within = (offset % bs) as usize;
        if within == 0 && buf.len() == self.block_bytes() {
            if let Some(data) = self.read_block_resolved(block_index).map_err(io_err)? {
                buf.copy_from_slice(&data);
                return Ok(());
            }
            return self.read_base(buf, offset);
        }
        let mut block = vec![0u8; self.block_bytes()];
        if let Some(data) = self.read_block_resolved(block_index).map_err(io_err)? {
            block.copy_from_slice(&data);
        } else {
            self.read_base(&mut block, block_index * bs)?;
        }
        buf.copy_from_slice(&block[within..within + buf.len()]);
        Ok(())
    }

    fn write_span(&mut self, layer_index: usize, buf: &[u8], offset: u64) -> io::Result<()> {
        let bs = u64::from(self.block_size);
        let block_index = offset / bs;
        let within = (offset % bs) as usize;
        let mut block = self
            .read_block_for_cow(layer_index, block_index)
            .map_err(io_err)?;
        block[within..within + buf.len()].copy_from_slice(buf);
        let layer = self
            .layers
            .get_mut(layer_index)
            .ok_or_else(|| io::Error::other("missing delta layer"))?;
        layer.write_block(block_index, &block).map_err(io_err)?;
        Ok(())
    }

    fn read_block_for_cow(
        &self,
        top_index: usize,
        block_index: u64,
    ) -> Result<Vec<u8>, SnapshotError> {
        let mut block = vec![0u8; self.block_bytes()];
        if self.layers[top_index].has_block(block_index) {
            self.layers[top_index].read_block(block_index, &mut block)?;
            return Ok(block);
        }
        if let Some(data) = self.read_block_resolved_below(top_index, block_index)? {
            return Ok(data);
        }
        let bs = u64::from(self.block_size);
        self.read_base(&mut block, block_index * bs)
            .map_err(|source| SnapshotError::Io {
                path: self.base_path.clone(),
                source,
            })?;
        Ok(block)
    }

    fn read_block_resolved(&self, block_index: u64) -> Result<Option<Vec<u8>>, SnapshotError> {
        self.read_block_resolved_below(self.layers.len(), block_index)
    }

    fn read_block_resolved_below(
        &self,
        below: usize,
        block_index: u64,
    ) -> Result<Option<Vec<u8>>, SnapshotError> {
        let end = below.min(self.layers.len());
        for layer in self.layers[..end].iter().rev() {
            if layer.has_block(block_index) {
                let mut block = vec![0u8; self.block_bytes()];
                layer.read_block(block_index, &mut block)?;
                return Ok(Some(block));
            }
        }
        Ok(None)
    }

    fn read_base(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        if offset.saturating_add(buf.len() as u64) > self.virtual_size {
            return Err(io::Error::new(ErrorKind::InvalidInput, "read past end"));
        }
        let mut done = 0;
        while done < buf.len() {
            let n = self.base.read_at(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                buf[done..].fill(0);
                return Ok(());
            }
            done += n;
        }
        Ok(())
    }
}

fn io_err(error: SnapshotError) -> io::Error {
    io::Error::other(error.to_string())
}
