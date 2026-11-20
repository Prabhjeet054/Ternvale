//! Host webcam → guest agent (`camera_frame`). Never logs pixel bytes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ternvale_agent_proto::{HexBytes, Message, MAX_BIN_CHUNK};
use ternvale_audio_host::open_camera;
use ternvale_devices::AgentServer;

/// Pump RGB24 frames to the connected agent until `stop`.
#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
pub fn spawn_camera_pump(
    agent: Arc<AgentServer>,
    streaming: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    match thread::Builder::new()
        .name("ternvale-camera".into())
        .spawn(move || pump(agent, streaming, stop))
    {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::error!(
                target: "ternvale::gpu",
                error = %error,
                "could not spawn camera pump"
            );
            None
        }
    }
}

fn pump(agent: Arc<AgentServer>, streaming: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
    let mut source = match open_camera() {
        Ok(source) => source,
        Err(denied) => {
            tracing::warn!(
                target: "ternvale::gpu",
                reason = %denied.reason,
                "host camera denied; guest will see no webcam"
            );
            streaming.store(false, Ordering::Relaxed);
            return;
        }
    };
    tracing::info!(
        target: "ternvale::gpu",
        "camera pump started (agent transport; pixels not logged)"
    );
    while !stop.load(Ordering::Relaxed) {
        match source.next_frame() {
            Some(frame) => {
                streaming.store(true, Ordering::Relaxed);
                send_frame(&agent, &frame);
            }
            None => streaming.store(false, Ordering::Relaxed),
        }
        thread::sleep(Duration::from_millis(100));
    }
    source.close();
    streaming.store(false, Ordering::Relaxed);
    tracing::info!(target: "ternvale::gpu", "camera pump stopped");
}

#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(seq = frame.seq, width = frame.width, height = frame.height, bytes = frame.pixels.len()))]
fn send_frame(agent: &AgentServer, frame: &ternvale_audio_host::CameraFrame) {
    let mut offset = 0u64;
    let total = frame.pixels.len();
    while offset < total as u64 {
        let start = offset as usize;
        let end = (start + MAX_BIN_CHUNK).min(total);
        let last = end == total;
        let message = Message::CameraFrame {
            seq: frame.seq,
            offset,
            last,
            width: frame.width,
            height: frame.height,
            format: frame.format.to_string(),
            data: HexBytes(frame.pixels[start..end].to_vec()),
        };
        match agent.send(&message) {
            Ok(()) => {}
            Err(error) => {
                tracing::debug!(
                    target: "ternvale::gpu",
                    error = %error,
                    seq = frame.seq,
                    "camera_frame not delivered (agent may be disconnected)"
                );
                break;
            }
        }
        offset = end as u64;
    }
}

#[cfg(test)]
mod tests {
    use ternvale_agent_proto::MAX_BIN_CHUNK;

    #[test]
    fn frames_are_split_to_agent_chunk_limit() {
        let len = MAX_BIN_CHUNK * 2 + 10;
        let mut offset = 0usize;
        let mut chunks = 0;
        while offset < len {
            let end = (offset + MAX_BIN_CHUNK).min(len);
            chunks += 1;
            offset = end;
        }
        assert_eq!(chunks, 3);
    }
}
