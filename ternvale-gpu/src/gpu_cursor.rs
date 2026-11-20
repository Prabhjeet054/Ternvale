//! virtio-gpu `cursorq`: host overlay from `UPDATE_CURSOR` / `MOVE_CURSOR`.
//!
//! Layout is `struct virtio_gpu_update_cursor` (virtio 1.2 §5.7.6.9 / Linux
//! `virtio_gpu.h`): 24-byte header plus 32-byte body (`cursor_pos`, resource,
//! hotspot). The host does **not** blit the cursor into scanout pixels; the
//! presenter streams the bitmap as a TVDP `KIND_CURSOR` overlay so the viewer
//! can use `NSCursor` (no extra frame of software-cursor lag).

use crate::gpu2d::Gpu2d;
use crate::protocol::{self, Command};
use crate::resource::BYTES_PER_PIXEL;

/// Largest cursor resource accepted (QEMU's cap). Larger bitmaps are dropped.
pub const MAX_CURSOR: u32 = 256;

/// Parse a cursor-queue request, update the overlay, ACK with `OK_NODATA`.
#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
pub(crate) fn handle(gpu: &mut Gpu2d, request: &[u8]) -> Vec<u8> {
    let (hdr, command) = match protocol::parse(request) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(
                target: "ternvale::gpu",
                error = %error,
                "malformed virtio-gpu cursor request"
            );
            return match protocol::parse_hdr(request) {
                Ok(hdr) => protocol::response_hdr(&hdr, protocol::RESP_ERR_UNSPEC).to_vec(),
                Err(_) => Vec::new(),
            };
        }
    };
    match command {
        Command::Cursor {
            scanout_id,
            x,
            y,
            resource_id,
            hot_x,
            hot_y,
            update,
        } => apply(gpu, scanout_id, x, y, resource_id, hot_x, hot_y, update),
        other => {
            tracing::warn!(
                target: "ternvale::gpu",
                cmd = protocol::type_name(hdr.kind),
                ?other,
                "non-cursor command on cursorq ignored"
            );
        }
    }
    tracing::debug!(
        target: "ternvale::gpu",
        cmd = protocol::type_name(hdr.kind),
        "virtio-gpu cursor overlay updated"
    );
    protocol::response_hdr(&hdr, protocol::RESP_OK_NODATA).to_vec()
}

#[allow(clippy::too_many_arguments)]
fn apply(
    gpu: &mut Gpu2d,
    scanout_id: u32,
    x: u32,
    y: u32,
    resource_id: u32,
    hot_x: u32,
    hot_y: u32,
    update: bool,
) {
    let mut overlay = gpu.display().cursor();
    overlay.scanout = scanout_id;
    overlay.x = x;
    overlay.y = y;
    if !update {
        overlay.sequence = overlay.sequence.saturating_add(1);
        gpu.display().set_cursor(overlay);
        tracing::trace!(
            target: "ternvale::gpu",
            scanout_id,
            x,
            y,
            "MOVE_CURSOR"
        );
        return;
    }
    overlay.hot_x = hot_x;
    overlay.hot_y = hot_y;
    if resource_id == 0 {
        overlay.visible = false;
        overlay.width = 0;
        overlay.height = 0;
        overlay.pixels.clear();
        overlay.sequence = overlay.sequence.saturating_add(1);
        gpu.display().set_cursor(overlay);
        tracing::debug!(target: "ternvale::gpu", scanout_id, "cursor hidden");
        return;
    }
    let Some(resource) = gpu.resource_ref(resource_id) else {
        tracing::warn!(
            target: "ternvale::gpu",
            resource_id,
            "UPDATE_CURSOR resource missing; overlay unchanged"
        );
        return;
    };
    if resource.width == 0
        || resource.height == 0
        || resource.width > MAX_CURSOR
        || resource.height > MAX_CURSOR
    {
        tracing::warn!(
            target: "ternvale::gpu",
            resource_id,
            width = resource.width,
            height = resource.height,
            max = MAX_CURSOR,
            "UPDATE_CURSOR bitmap rejected"
        );
        return;
    }
    if hot_x >= resource.width || hot_y >= resource.height {
        tracing::warn!(
            target: "ternvale::gpu",
            resource_id,
            hot_x,
            hot_y,
            width = resource.width,
            height = resource.height,
            "UPDATE_CURSOR hotspot outside the bitmap"
        );
        return;
    }
    overlay.visible = true;
    overlay.width = resource.width;
    overlay.height = resource.height;
    overlay.format = resource.format.to_wire();
    overlay.pixels = resource.pixels.clone();
    overlay.sequence = overlay.sequence.saturating_add(1);
    let bytes = overlay.pixels.len();
    gpu.display().set_cursor(overlay);
    tracing::debug!(
        target: "ternvale::gpu",
        resource_id,
        width = resource.width,
        height = resource.height,
        hot_x,
        hot_y,
        bpp = BYTES_PER_PIXEL,
        bytes,
        "UPDATE_CURSOR overlay"
    );
}

#[cfg(test)]
#[path = "gpu_cursor_tests.rs"]
mod tests;
