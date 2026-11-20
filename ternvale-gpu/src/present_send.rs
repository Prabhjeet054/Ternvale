//! Write one TVDP frame (full or packed damage) to a viewer.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::Instant;

use crate::present::{PresentConfig, PresentStats};
use crate::present_protocol::{FrameHeader, FRAME_FLAG_DISPLAY_P3, FRAME_FLAG_PARTIAL};
use crate::present_session::{io, InFlight, SessionError};
use crate::present_window::LatencyWindow;
use crate::scanout::GpuDisplay;

/// Next scanout whose sequence has moved since we last sent it.
#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
pub(crate) fn next_dirty_scanout(
    display: &GpuDisplay,
    config: PresentConfig,
    last_sent: &[Option<u64>],
) -> Option<usize> {
    let dirty = |scanout: usize| {
        let sequence = display.sequence(scanout).unwrap_or(0);
        last_sent.get(scanout).copied().flatten() != Some(sequence)
    };
    if config.all_scanouts {
        (0..display.scanouts()).find(|&scanout| dirty(scanout))
    } else {
        dirty(config.scanout).then_some(config.scanout)
    }
}

/// Encode the scanout and write header + payload. At most one caller holds
/// an unacked frame.
#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(scanout))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn send_frame(
    stream: &mut UnixStream,
    display: &GpuDisplay,
    config: PresentConfig,
    scanout: usize,
    last_sent: Option<u64>,
    waited_on_viewer: bool,
    stats: &PresentStats,
    window: &mut LatencyWindow,
    metrics: Option<&ternvale_metrics::Metrics>,
) -> Result<InFlight, SessionError> {
    let packed = display.with_frame(scanout, |frame| {
        if let Some(last) = last_sent {
            let dropped = frame.sequence.saturating_sub(last).saturating_sub(1);
            if dropped > 0 {
                stats
                    .dropped
                    .fetch_add(dropped, std::sync::atomic::Ordering::Relaxed);
                window.dropped(dropped);
                let reason = if waited_on_viewer {
                    "viewer busy"
                } else {
                    "fps cap"
                };
                tracing::debug!(
                    target: "ternvale::gpu",
                    count = dropped,
                    reason,
                    last_sent = last,
                    sending = frame.sequence,
                    scanout,
                    "frames dropped"
                );
                if let Some(metrics) = metrics {
                    metrics.record_present(0, dropped, 0, None);
                }
            }
        }
        let (damage, pixels, partial) = frame.pack_dirty();
        frame.dirty = None;
        PackedFrame {
            width: frame.width,
            height: frame.height,
            stride: frame.stride(),
            format: frame.format.map_or(0, |f| f.to_wire()),
            sequence: frame.sequence,
            updated: frame.updated,
            damage,
            pixels,
            partial,
        }
    });
    let Some(packed) = packed else {
        return Err(SessionError::Frame {
            width: 0,
            height: 0,
            bytes: 0,
        });
    };
    let too_big = || SessionError::Frame {
        width: packed.width,
        height: packed.height,
        bytes: packed.pixels.len(),
    };
    let mut flags = 0;
    if packed.partial {
        flags |= FRAME_FLAG_PARTIAL;
    }
    if config.quality.display_p3() {
        flags |= FRAME_FLAG_DISPLAY_P3;
    }
    let damage_w = if packed.damage.width == 0 {
        packed.width
    } else {
        packed.damage.width
    };
    let damage_h = if packed.damage.height == 0 {
        packed.height
    } else {
        packed.damage.height
    };
    let header = FrameHeader {
        width: packed.width,
        height: packed.height,
        stride: u32::try_from(packed.stride).map_err(|_| too_big())?,
        format: packed.format,
        sequence: packed.sequence,
        payload_len: u32::try_from(packed.pixels.len()).map_err(|_| too_big())?,
        flags,
        scanout: scanout as u32,
        damage_x: packed.damage.x,
        damage_y: packed.damage.y,
        damage_w,
        damage_h,
        scale_x100: config.quality.scale_x100.max(1),
    };
    let started = Instant::now();
    stream
        .write_all(&header.encode())
        .and_then(|()| stream.write_all(&packed.pixels))
        .map_err(io("write frame to viewer"))?;
    let upload = u64::from(header.payload_len);
    stats
        .sent
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    stats
        .upload_bytes
        .fetch_add(upload, std::sync::atomic::Ordering::Relaxed);
    window.uploaded(upload);
    if let Some(metrics) = metrics {
        metrics.record_present(1, 0, upload, None);
    }
    tracing::trace!(
        target: "ternvale::gpu",
        sequence = packed.sequence,
        scanout,
        width = packed.width,
        height = packed.height,
        bytes = packed.pixels.len(),
        partial = packed.partial,
        damage = %packed.damage,
        "frame sent"
    );
    Ok(InFlight {
        sequence: packed.sequence,
        sent_at: started,
        updated: packed.updated,
        warned: false,
        upload_bytes: upload,
    })
}

struct PackedFrame {
    width: u32,
    height: u32,
    stride: usize,
    format: u32,
    sequence: u64,
    updated: Option<Instant>,
    damage: crate::protocol::Rect,
    pixels: Vec<u8>,
    partial: bool,
}
