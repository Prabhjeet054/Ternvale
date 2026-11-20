//! Reassemble hostile `window_frame` streams from the guest agent.

use std::collections::HashMap;

use ternvale_agent_proto::{MAX_BIN_CHUNK, MAX_WINDOW_FRAME_BYTES};

/// One completed per-window capture (pixels never logged).
#[derive(Clone)]
pub struct WindowPixels {
    /// Guest window id.
    pub id: u32,
    /// Capture sequence.
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    /// `rgb24`.
    pub format: String,
    /// Raw pixels.
    pub data: Vec<u8>,
}

struct Partial {
    seq: u64,
    width: u32,
    height: u32,
    format: String,
    buf: Vec<u8>,
}

/// In-order chunk assembler keyed by window id.
#[derive(Default)]
pub struct FrameAssembler {
    partial: HashMap<u32, Partial>,
    done: Vec<WindowPixels>,
}

impl FrameAssembler {
    /// Apply one validated chunk. Drops streams that skip offsets or overflow.
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(
        level = "trace",
        target = "ternvale::agent",
        skip_all,
        fields(id, seq, offset, last, bytes)
    )]
    pub fn push(
        &mut self,
        id: u32,
        seq: u64,
        offset: u64,
        last: bool,
        width: u32,
        height: u32,
        format: String,
        data: Vec<u8>,
    ) {
        if data.len() > MAX_BIN_CHUNK {
            tracing::warn!(
                target: "ternvale::agent",
                id,
                bytes = data.len(),
                "window_frame chunk rejected (too long)"
            );
            self.partial.remove(&id);
            return;
        }
        let entry = self.partial.entry(id).or_insert_with(|| Partial {
            seq,
            width,
            height,
            format: format.clone(),
            buf: Vec::new(),
        });
        if entry.seq != seq {
            entry.seq = seq;
            entry.width = width;
            entry.height = height;
            entry.format = format.clone();
            entry.buf.clear();
        }
        if entry.buf.len() as u64 != offset {
            tracing::warn!(
                target: "ternvale::agent",
                id,
                seq,
                offset,
                have = entry.buf.len(),
                "window_frame offset mismatch; dropping stream"
            );
            self.partial.remove(&id);
            return;
        }
        if entry.buf.len().saturating_add(data.len()) > MAX_WINDOW_FRAME_BYTES {
            tracing::warn!(
                target: "ternvale::agent",
                id,
                seq,
                "window_frame exceeded MAX_WINDOW_FRAME_BYTES"
            );
            self.partial.remove(&id);
            return;
        }
        entry.buf.extend_from_slice(&data);
        if last {
            if let Some(done) = self.partial.remove(&id) {
                tracing::debug!(
                    target: "ternvale::agent",
                    id,
                    seq,
                    bytes = done.buf.len(),
                    width = done.width,
                    height = done.height,
                    "window_frame assembled (pixels not logged)"
                );
                self.done.push(WindowPixels {
                    id,
                    seq: done.seq,
                    width: done.width,
                    height: done.height,
                    format: done.format,
                    data: done.buf,
                });
            }
        }
    }

    /// Drain completed frames.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn take(&mut self) -> Vec<WindowPixels> {
        std::mem::take(&mut self.done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_two_chunks_and_rejects_gap() {
        let mut asm = FrameAssembler::default();
        asm.push(1, 1, 0, false, 2, 1, "rgb24".into(), vec![1, 2, 3]);
        asm.push(1, 1, 3, true, 2, 1, "rgb24".into(), vec![4, 5, 6]);
        let frames = asm.take();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, vec![1, 2, 3, 4, 5, 6]);
        asm.push(2, 1, 0, false, 1, 1, "rgb24".into(), vec![9]);
        asm.push(2, 1, 3, true, 1, 1, "rgb24".into(), vec![8]);
        assert!(asm.take().is_empty());
    }
}
