//! Assemble host webcam chunks and write them for a guest loopback helper.
//! Pixel bytes are never logged.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ternvale_agent_proto::Message;

use crate::AgentError;

/// Default dest (`v4l2loopback` helpers can mmap this).
pub const DEFAULT_FRAME_PATH: &str = "/tmp/ternvale-camera.rgb";

struct Assembly {
    seq: u64,
    width: u32,
    height: u32,
    buf: Vec<u8>,
}

static ASSEMBLY: Mutex<Option<Assembly>> = Mutex::new(None);

/// Handle `camera_frame`. Writes a complete RGB24 frame atomically.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(seq, offset, last, bytes)
)]
pub fn handle(message: &Message) -> Result<(), AgentError> {
    let Message::CameraFrame {
        seq,
        offset,
        last,
        width,
        height,
        format,
        data,
    } = message
    else {
        return Ok(());
    };
    tracing::Span::current().record("seq", seq);
    tracing::Span::current().record("offset", offset);
    tracing::Span::current().record("last", last);
    tracing::Span::current().record("bytes", data.0.len());
    if format != "rgb24" {
        tracing::warn!(
            target: "ternvale::agent",
            format = %format,
            "camera_frame ignored (unsupported format)"
        );
        return Ok(());
    }
    let mut slot = ASSEMBLY.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_none_or(|a| a.seq != *seq) {
        *slot = Some(Assembly {
            seq: *seq,
            width: *width,
            height: *height,
            buf: Vec::new(),
        });
    }
    let Some(asm) = slot.as_mut() else {
        return Ok(());
    };
    let start = *offset as usize;
    let end = start.saturating_add(data.0.len());
    // 1280×720 RGB24 is 2_764_800 bytes; keep headroom under 4 MiB.
    const MAX: usize = 4 * 1024 * 1024;
    if end > MAX {
        tracing::warn!(
            target: "ternvale::agent",
            seq,
            end,
            "camera_frame dropped (too large)"
        );
        *slot = None;
        return Ok(());
    }
    if asm.buf.len() < end {
        asm.buf.resize(end, 0);
    }
    asm.buf[start..end].copy_from_slice(&data.0);
    if *last {
        let path = frame_path();
        write_atomic(&path, &asm.buf)?;
        tracing::debug!(
            target: "ternvale::agent",
            seq,
            width = asm.width,
            height = asm.height,
            bytes = asm.buf.len(),
            path = %path.display(),
            "camera frame written"
        );
        *slot = None;
    }
    Ok(())
}

fn frame_path() -> PathBuf {
    std::env::var_os("TERNVALE_CAMERA_FRAME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_FRAME_PATH))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AgentError> {
    let tmp = path.with_extension("rgb.part");
    fs::write(&tmp, bytes).map_err(|source| AgentError::Action {
        action: "write camera frame",
        source,
    })?;
    fs::rename(&tmp, path).map_err(|source| AgentError::Action {
        action: "rename camera frame",
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_agent_proto::HexBytes;

    #[test]
    fn assembles_chunks_without_logging_pixels() {
        let dir = std::env::temp_dir().join(format!("tv-cam-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let dest = dir.join("frame.rgb");
        std::env::set_var("TERNVALE_CAMERA_FRAME", &dest);
        handle(&Message::CameraFrame {
            seq: 1,
            offset: 0,
            last: false,
            width: 2,
            height: 1,
            format: "rgb24".into(),
            data: HexBytes(vec![1, 2, 3]),
        })
        .expect("first");
        handle(&Message::CameraFrame {
            seq: 1,
            offset: 3,
            last: true,
            width: 2,
            height: 1,
            format: "rgb24".into(),
            data: HexBytes(vec![4, 5, 6]),
        })
        .expect("last");
        assert_eq!(fs::read(&dest).expect("read"), [1, 2, 3, 4, 5, 6]);
        std::env::remove_var("TERNVALE_CAMERA_FRAME");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn assembles_720p_preview_without_logging_pixels() {
        let dir = std::env::temp_dir().join(format!("tv-cam720-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let dest = dir.join("preview.rgb");
        std::env::set_var("TERNVALE_CAMERA_FRAME", &dest);
        let width = 1280u32;
        let height = 720u32;
        let pixels = vec![0x7au8; (width * height * 3) as usize];
        let seq = 720;
        let mut offset = 0u64;
        while (offset as usize) < pixels.len() {
            let start = offset as usize;
            let end = (start + ternvale_agent_proto::MAX_BIN_CHUNK).min(pixels.len());
            let last = end == pixels.len();
            handle(&Message::CameraFrame {
                seq,
                offset,
                last,
                width,
                height,
                format: "rgb24".into(),
                data: HexBytes(pixels[start..end].to_vec()),
            })
            .expect("chunk");
            offset = end as u64;
        }
        assert_eq!(fs::read(&dest).expect("read").len(), pixels.len());
        std::env::remove_var("TERNVALE_CAMERA_FRAME");
        let _ = fs::remove_dir_all(&dir);
    }
}
