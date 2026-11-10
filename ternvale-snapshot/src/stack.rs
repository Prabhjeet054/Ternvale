//! Layered image: base raw file + ordered deltas (newest wins on read).

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use ternvale_crypto::DiskXts;

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
    xts: Option<DiskXts>,
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
        Self::open_crypto(manifest_path, read_only, None)
    }

    /// [`open`] with optional AES-256-XTS on layer and base payloads (not headers).
    pub fn open_crypto(
        manifest_path: &Path,
        read_only: bool,
        xts: Option<DiskXts>,
    ) -> Result<Self, SnapshotError> {
        let manifest = StackManifest::load(manifest_path)?;
        Self::open_manifest_crypto(&manifest, read_only, xts)
    }

    /// Open from a parsed manifest.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(read_only)
    )]
    pub fn open_manifest(manifest: &StackManifest, read_only: bool) -> Result<Self, SnapshotError> {
        Self::open_manifest_crypto(manifest, read_only, None)
    }

    /// [`open_manifest`] with optional AES-256-XTS on payloads.
    pub fn open_manifest_crypto(
        manifest: &StackManifest,
        read_only: bool,
        xts: Option<DiskXts>,
    ) -> Result<Self, SnapshotError> {
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
            encrypted = xts.is_some(),
            "image stack ready"
        );
        Ok(Self {
            base,
            base_path: manifest.base.clone(),
            layers,
            virtual_size,
            block_size,
            read_only,
            xts,
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
        Self::open_manifest_crypto(&manifest, read_only, None)
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

    /// Discard or zero `len` bytes at `offset`. Full COW blocks with `unmap`
    /// are marked unmapped/zero in the top delta; partial ranges are written
    /// as zeros.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(offset, len, unmap)
    )]
    pub fn discard(&mut self, offset: u64, len: u64, unmap: bool) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "read-only stack",
            ));
        }
        if offset.saturating_add(len) > self.virtual_size {
            return Err(io::Error::new(ErrorKind::InvalidInput, "discard past end"));
        }
        if self.layers.is_empty() {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "stack has no writable delta layer",
            ));
        }
        let bs = u64::from(self.block_size);
        let mut pos = offset;
        let end = offset + len;
        while pos < end {
            let block_index = pos / bs;
            let block_start = block_index * bs;
            let block_end = (block_start + bs).min(self.virtual_size);
            let span_end = end.min(block_end);
            if unmap && pos == block_start && span_end == block_end {
                let top = self.layers.len() - 1;
                self.layers[top].unmap_block(block_index).map_err(io_err)?;
                pos = span_end;
                continue;
            }
            let zeros = vec![0u8; (span_end - pos) as usize];
            self.write_at(&zeros, pos)?;
            pos = span_end;
        }
        Ok(())
    }

    /// Enlarge the guest-visible size (new space reads as zeros).
    pub fn grow(&mut self, bytes: u64) -> Result<(), SnapshotError> {
        if self.read_only {
            return Err(SnapshotError::ReadOnly {
                path: self.base_path.clone(),
            });
        }
        let aligned = bytes - (bytes % u64::from(self.block_size));
        if aligned < self.virtual_size {
            return Err(SnapshotError::InvalidRequest {
                detail: "stack grow cannot shrink".into(),
            });
        }
        if let Some(top) = self.layers.last_mut() {
            top.grow(aligned)?;
        }
        self.virtual_size = aligned;
        tracing::info!(
            target = "ternvale::snapshot",
            bytes = aligned,
            "layered disk grown"
        );
        Ok(())
    }

    /// Rewrite the top delta, dropping unmapped packed blocks.
    pub fn compact_top(&mut self) -> Result<(u64, u64), SnapshotError> {
        let Some(top) = self.layers.last_mut() else {
            return Err(SnapshotError::InvalidRequest {
                detail: "stack has no delta to compact".into(),
            });
        };
        top.compact()
    }

    /// Compact every delta layer, dropping unmapped packed blocks.
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all)]
    pub fn compact_layers(&mut self) -> Result<(u64, u64), SnapshotError> {
        let before = self.allocated_bytes()?;
        for layer in &mut self.layers {
            layer.compact()?;
        }
        let after = self.allocated_bytes()?;
        tracing::info!(
            target = "ternvale::snapshot",
            before,
            after,
            "layered disk compacted"
        );
        Ok((before, after))
    }

    /// Grow or shrink the guest-visible size.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::snapshot",
        skip_all,
        fields(bytes)
    )]
    pub fn resize(&mut self, bytes: u64) -> Result<(), SnapshotError> {
        let aligned = bytes - (bytes % u64::from(self.block_size));
        if aligned >= self.virtual_size {
            return self.grow(aligned);
        }
        if let Some(top) = self.layers.last_mut() {
            top.shrink(aligned)?;
        }
        self.virtual_size = aligned;
        tracing::info!(
            target = "ternvale::snapshot",
            bytes = aligned,
            "layered disk shrunk"
        );
        Ok(())
    }

    /// Host allocated bytes across base and deltas (`st_blocks` * 512).
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all)]
    pub fn allocated_bytes(&self) -> Result<u64, SnapshotError> {
        let mut total = crate::delta::allocated_bytes(&self.base_path)?;
        for layer in &self.layers {
            total += crate::delta::allocated_bytes(layer.path())?;
        }
        Ok(total)
    }

    /// Highest byte that holds guest data (allocated, non-unmapped).
    pub fn in_use_bytes(&self) -> u64 {
        let mut max = 0u64;
        let bs = u64::from(self.block_size);
        let blocks = self.virtual_size.div_ceil(bs);
        for index in 0..blocks {
            let mut used = false;
            for layer in self.layers.iter().rev() {
                if layer.has_zero(index) {
                    used = false;
                    break;
                }
                if layer.has_block(index) {
                    used = true;
                    break;
                }
            }
            if used {
                max = (index + 1) * bs;
            }
        }
        max.min(self.virtual_size)
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
            return self.read_base_plain(buf, offset);
        }
        let mut block = vec![0u8; self.block_bytes()];
        if let Some(data) = self.read_block_resolved(block_index).map_err(io_err)? {
            block.copy_from_slice(&data);
        } else {
            self.read_base_plain(&mut block, block_index * bs)?;
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
        self.xts_block(&mut block, block_index, true)
            .map_err(io_err)?;
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
            self.xts_block(&mut block, block_index, false)?;
            return Ok(block);
        }
        if let Some(data) = self.read_block_resolved_below(top_index, block_index)? {
            return Ok(data);
        }
        let bs = u64::from(self.block_size);
        self.read_base_plain(&mut block, block_index * bs)
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
            if layer.has_zero(block_index) {
                return Ok(Some(vec![0u8; self.block_bytes()]));
            }
            if layer.has_block(block_index) {
                let mut block = vec![0u8; self.block_bytes()];
                layer.read_block(block_index, &mut block)?;
                self.xts_block(&mut block, block_index, false)?;
                return Ok(Some(block));
            }
        }
        Ok(None)
    }

    fn xts_block(
        &self,
        buf: &mut [u8],
        block_index: u64,
        encrypt: bool,
    ) -> Result<(), SnapshotError> {
        let Some(xts) = &self.xts else {
            return Ok(());
        };
        let offset = block_index * u64::from(self.block_size);
        xts.crypt(buf, offset, encrypt)
            .map_err(|error| SnapshotError::InvalidRequest {
                detail: error.to_string(),
            })
    }

    fn read_base_plain(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        if offset.saturating_add(buf.len() as u64) > self.virtual_size {
            return Err(io::Error::new(ErrorKind::InvalidInput, "read past end"));
        }
        if self.xts.is_none() {
            return self.read_base_raw(buf, offset);
        }
        let (cover_off, cover_len) = ternvale_crypto::covering_range(offset, buf.len());
        if cover_off + cover_len as u64 > self.virtual_size {
            return Err(io::Error::new(ErrorKind::InvalidInput, "read past end"));
        }
        let mut cover = vec![0u8; cover_len];
        self.read_base_raw(&mut cover, cover_off)?;
        let xts = self.xts.as_ref().expect("xts");
        xts.crypt(&mut cover, cover_off, false)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let skip = (offset - cover_off) as usize;
        buf.copy_from_slice(&cover[skip..skip + buf.len()]);
        Ok(())
    }

    fn read_base_raw(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
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
