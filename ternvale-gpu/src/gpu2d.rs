//! The 2D command processor: resources, scanouts, and one response per request.
//!
//! Commands follow virtio 1.2 §5.7.6.8. Every guest value (ids, rectangles,
//! offsets, backing pages, sizes) is checked before use; a bad one is a WARN
//! and an error response, never a panic. Every command is logged at DEBUG;
//! every flush that reaches a scanout logs its timing at TRACE.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ternvale_vmm::GuestMemory;

use crate::protocol::{self, Command, ParseError, Rect};
use crate::resource::{Backing, PixelFormat, Resource, BYTES_PER_PIXEL};
use crate::scanout::{self, GpuDisplay, ScanoutState};

/// Host memory all resources together may use (QEMU's `max_hostmem` default).
pub const MAX_HOSTMEM: u64 = 256 << 20;
/// Largest width or height of one resource.
pub const MAX_DIMENSION: u32 = 16_384;

/// Counters for one virtio-gpu device.
#[derive(Debug, Default)]
pub struct GpuStats {
    /// Control commands answered, including errors.
    pub commands: AtomicU64,
    /// Commands answered with an error.
    pub errors: AtomicU64,
    /// Flushes that reached at least one scanout.
    pub flushes: AtomicU64,
    /// Bytes copied from guest backing by `TRANSFER_TO_HOST_2D`.
    pub transferred: AtomicU64,
}

impl GpuStats {
    /// `(commands, errors, flushes, transferred bytes)`.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn snapshot(&self) -> [u64; 4] {
        [
            self.commands.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
            self.flushes.load(Ordering::Relaxed),
            self.transferred.load(Ordering::Relaxed),
        ]
    }
}

/// Error response type plus the reason logged at WARN.
type Fail = (u32, String);

/// 2D device state. Owned by the control-queue worker.
pub struct Gpu2d {
    display: GpuDisplay,
    resources: HashMap<u32, Resource>,
    scanouts: Vec<ScanoutState>,
    hostmem: u64,
    stats: Arc<GpuStats>,
}

impl Gpu2d {
    /// No resources and every scanout disabled.
    pub fn new(display: GpuDisplay, stats: Arc<GpuStats>) -> Self {
        let scanouts = vec![ScanoutState::default(); display.scanouts()];
        Self {
            display,
            resources: HashMap::new(),
            scanouts,
            hostmem: 0,
            stats,
        }
    }

    /// Device reset: drop every resource and disable every scanout.
    pub fn reset(&mut self) {
        let resources = self.resources.len();
        self.resources.clear();
        self.hostmem = 0;
        for (index, state) in self.scanouts.iter_mut().enumerate() {
            if state.resource_id != 0 {
                self.display.with_frame(index, scanout::blank);
            }
            *state = ScanoutState::default();
        }
        tracing::debug!(target: "ternvale::gpu", resources, "virtio-gpu 2d state reset");
    }

    #[cfg(test)]
    fn hostmem(&self) -> u64 {
        self.hostmem
    }

    /// Answer one control request. Empty when the request has no header to answer.
    pub fn handle(&mut self, mem: &GuestMemory, request: &[u8]) -> Vec<u8> {
        self.stats.commands.fetch_add(1, Ordering::Relaxed);
        let (hdr, command) = match protocol::parse(request) {
            Ok(parsed) => parsed,
            Err(error) => return self.parse_failed(request, &error),
        };
        let name = protocol::type_name(hdr.kind);
        tracing::debug!(
            target: "ternvale::gpu",
            cmd = name,
            kind = format!("{:#06x}", hdr.kind),
            fenced = hdr.flags & protocol::FLAG_FENCE != 0,
            fence_id = hdr.fence_id,
            ?command,
            "virtio-gpu command"
        );
        let result = match command {
            Command::GetDisplayInfo => {
                return protocol::display_info(&hdr, self.display.modes());
            }
            Command::ResourceCreate2d {
                resource_id,
                format,
                width,
                height,
            } => self.create(resource_id, format, width, height),
            Command::ResourceUnref { resource_id } => self.unref(resource_id),
            Command::SetScanout {
                rect,
                scanout_id,
                resource_id,
            } => self.set_scanout(rect, scanout_id, resource_id),
            Command::ResourceFlush { rect, resource_id } => self.flush(rect, resource_id),
            Command::TransferToHost2d {
                rect,
                offset,
                resource_id,
            } => self.transfer(mem, rect, offset, resource_id),
            Command::ResourceAttachBacking {
                resource_id,
                entries,
            } => self.attach(mem, resource_id, entries),
            Command::ResourceDetachBacking { resource_id } => self.detach(resource_id),
            Command::Unsupported => Err((
                protocol::RESP_ERR_UNSPEC,
                "not implemented by the 2D-only device".to_string(),
            )),
        };
        let kind = match result {
            Ok(()) => protocol::RESP_OK_NODATA,
            Err((kind, reason)) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    target: "ternvale::gpu",
                    cmd = name,
                    response = protocol::type_name(kind),
                    reason = %reason,
                    "virtio-gpu command failed"
                );
                kind
            }
        };
        protocol::response_hdr(&hdr, kind).to_vec()
    }

    fn parse_failed(&mut self, request: &[u8], error: &ParseError) -> Vec<u8> {
        self.stats.errors.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(target: "ternvale::gpu", len = request.len(), error = %error, "malformed virtio-gpu request");
        let Ok(hdr) = protocol::parse_hdr(request) else {
            return Vec::new();
        };
        let kind = match error {
            ParseError::TooManyEntries { .. } => protocol::RESP_ERR_INVALID_PARAMETER,
            _ => protocol::RESP_ERR_UNSPEC,
        };
        protocol::response_hdr(&hdr, kind).to_vec()
    }

    fn resource(&mut self, id: u32) -> Result<&mut Resource, Fail> {
        self.resources.get_mut(&id).ok_or_else(|| {
            (
                protocol::RESP_ERR_INVALID_RESOURCE_ID,
                format!("no resource {id}"),
            )
        })
    }

    fn create(&mut self, id: u32, format: u32, width: u32, height: u32) -> Result<(), Fail> {
        if id == 0 || self.resources.contains_key(&id) {
            return Err((
                protocol::RESP_ERR_INVALID_RESOURCE_ID,
                format!("resource id {id} is 0 or in use"),
            ));
        }
        let Some(pixel_format) = PixelFormat::from_wire(format) else {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!("unknown format {format}"),
            ));
        };
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!("size {width}x{height} outside 1..={MAX_DIMENSION}"),
            ));
        }
        let bytes = u64::from(width) * u64::from(height) * u64::from(BYTES_PER_PIXEL);
        if self.hostmem + bytes > MAX_HOSTMEM {
            return Err((
                protocol::RESP_ERR_OUT_OF_MEMORY,
                format!(
                    "{bytes} bytes would pass the {MAX_HOSTMEM}-byte budget ({} used)",
                    self.hostmem
                ),
            ));
        }
        self.resources
            .insert(id, Resource::new(pixel_format, width, height));
        self.hostmem += bytes;
        tracing::debug!(target: "ternvale::gpu", resource_id = id, format = pixel_format.name(), width, height, bytes, hostmem = self.hostmem, "virtio-gpu resource created");
        Ok(())
    }

    fn unref(&mut self, id: u32) -> Result<(), Fail> {
        let Some(resource) = self.resources.remove(&id) else {
            return Err((
                protocol::RESP_ERR_INVALID_RESOURCE_ID,
                format!("no resource {id}"),
            ));
        };
        self.hostmem -= resource.pixels.len() as u64;
        for (index, state) in self.scanouts.iter_mut().enumerate() {
            if state.resource_id == id {
                *state = ScanoutState::default();
                self.display.with_frame(index, scanout::blank);
                tracing::info!(target: "ternvale::gpu", scanout = index, resource_id = id, "virtio-gpu scanout disabled (resource destroyed)");
            }
        }
        tracing::debug!(target: "ternvale::gpu", resource_id = id, hostmem = self.hostmem, "virtio-gpu resource destroyed");
        Ok(())
    }

    fn set_scanout(&mut self, rect: Rect, scanout_id: u32, id: u32) -> Result<(), Fail> {
        let index = scanout_id as usize;
        if index >= self.scanouts.len() {
            return Err((
                protocol::RESP_ERR_INVALID_SCANOUT_ID,
                format!("scanout {scanout_id} of {}", self.scanouts.len()),
            ));
        }
        if id == 0 {
            if self.scanouts[index].resource_id != 0 {
                tracing::info!(target: "ternvale::gpu", scanout = index, "virtio-gpu scanout disabled");
            }
            self.scanouts[index] = ScanoutState::default();
            self.display.with_frame(index, scanout::blank);
            return Ok(());
        }
        let resource = self.resources.get(&id).ok_or_else(|| {
            (
                protocol::RESP_ERR_INVALID_RESOURCE_ID,
                format!("no resource {id}"),
            )
        })?;
        if !rect.fits_in(resource.width, resource.height) {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!(
                    "rect {rect} outside resource {id} ({}x{})",
                    resource.width, resource.height
                ),
            ));
        }
        let previous = &self.scanouts[index];
        if previous.resource_id == 0
            || (previous.rect.width, previous.rect.height) != (rect.width, rect.height)
        {
            tracing::info!(target: "ternvale::gpu", scanout = index, resource_id = id, width = rect.width, height = rect.height, format = resource.format.name(), "virtio-gpu scanout enabled");
        }
        self.display
            .with_frame(index, |frame| scanout::show(frame, resource, rect));
        self.scanouts[index] = ScanoutState {
            resource_id: id,
            rect,
            last_flush: None,
        };
        Ok(())
    }

    fn flush(&mut self, rect: Rect, id: u32) -> Result<(), Fail> {
        let resource = self.resources.get(&id).ok_or_else(|| {
            (
                protocol::RESP_ERR_INVALID_RESOURCE_ID,
                format!("no resource {id}"),
            )
        })?;
        if !rect.fits_in(resource.width, resource.height) {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!(
                    "rect {rect} outside resource {id} ({}x{})",
                    resource.width, resource.height
                ),
            ));
        }
        let mut reached = false;
        for (index, state) in self.scanouts.iter_mut().enumerate() {
            if state.resource_id != id {
                continue;
            }
            let Some(damage) = rect.intersect(&state.rect) else {
                continue;
            };
            let started = Instant::now();
            let copied = self.display.with_frame(index, |frame| {
                let bytes = scanout::copy_into(frame, resource, state.rect, damage);
                frame.sequence += 1;
                (bytes, frame.sequence)
            });
            let Some((bytes, sequence)) = copied else {
                continue;
            };
            let since_last_us = state.last_flush.map(|at| at.elapsed().as_micros() as u64);
            state.last_flush = Some(Instant::now());
            reached = true;
            tracing::trace!(
                target: "ternvale::gpu",
                scanout = index,
                resource_id = id,
                damage = %damage,
                bytes,
                copy_us = started.elapsed().as_micros() as u64,
                since_last_us,
                sequence,
                "virtio-gpu frame flush"
            );
        }
        if reached {
            self.stats.flushes.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn transfer(
        &mut self,
        mem: &GuestMemory,
        rect: Rect,
        offset: u64,
        id: u32,
    ) -> Result<(), Fail> {
        let resource = self.resource(id)?;
        if !rect.fits_in(resource.width, resource.height) {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!(
                    "rect {rect} outside resource {id} ({}x{})",
                    resource.width, resource.height
                ),
            ));
        }
        let Some(backing) = resource.backing.take() else {
            return Err((
                protocol::RESP_ERR_UNSPEC,
                format!("resource {id} has no backing"),
            ));
        };
        let started = Instant::now();
        let result = resource.transfer_from(mem, &backing, rect, offset);
        resource.backing = Some(backing);
        let bytes = result.map_err(|error| {
            (
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!("resource {id} rect {rect} offset {offset:#x}: {error}"),
            )
        })?;
        self.stats.transferred.fetch_add(bytes, Ordering::Relaxed);
        tracing::trace!(target: "ternvale::gpu", resource_id = id, rect = %rect, offset = format!("{offset:#x}"), bytes, copy_us = started.elapsed().as_micros() as u64, "virtio-gpu transfer to host");
        Ok(())
    }

    fn attach(
        &mut self,
        mem: &GuestMemory,
        id: u32,
        entries: Vec<protocol::MemEntry>,
    ) -> Result<(), Fail> {
        let resource = self.resource(id)?;
        if resource.backing.is_some() {
            return Err((
                protocol::RESP_ERR_UNSPEC,
                format!("resource {id} already has backing"),
            ));
        }
        let backing = Backing::new(entries);
        if let Some(bad) = backing.first_unmapped(mem) {
            return Err((
                protocol::RESP_ERR_INVALID_PARAMETER,
                format!("resource {id} backing entry {bad} is outside guest memory"),
            ));
        }
        tracing::debug!(target: "ternvale::gpu", resource_id = id, entries = backing.len(), bytes = backing.total(), "virtio-gpu backing attached");
        resource.backing = Some(backing);
        Ok(())
    }

    fn detach(&mut self, id: u32) -> Result<(), Fail> {
        let resource = self.resource(id)?;
        if resource.backing.take().is_none() {
            return Err((
                protocol::RESP_ERR_UNSPEC,
                format!("resource {id} has no backing"),
            ));
        }
        tracing::debug!(target: "ternvale::gpu", resource_id = id, "virtio-gpu backing detached");
        Ok(())
    }
}

#[cfg(test)]
#[path = "gpu2d_tests.rs"]
mod tests;
