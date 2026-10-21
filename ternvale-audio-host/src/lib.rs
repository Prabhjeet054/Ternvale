//! Host playback sink for virtio-sound (CoreAudio on macOS through `cpal`).

mod cpal_sink;
mod null_sink;

pub use cpal_sink::CpalPlayback;
pub use null_sink::NullPlayback;

/// PCM layout agreed with the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleFormat {
    /// Sample rate in Hz.
    pub rate_hz: u32,
    /// Channel count (1 or 2 today).
    pub channels: u8,
    /// Bytes per interleaved frame (all channels).
    pub bytes_per_frame: usize,
}

/// Host playback error.
#[derive(Debug, thiserror::Error)]
pub enum HostAudioError {
    /// No output device or stream could be opened.
    #[error("host audio output unavailable: {message}")]
    Unavailable {
        /// Detail for logs.
        message: String,
    },
    /// Invalid parameters.
    #[error("host audio invalid format: {0}")]
    Invalid(String),
}

/// Push guest PCM into a host playback stream.
pub trait PlaybackSink: Send {
    /// (Re)open the output stream for `format`.
    fn configure(&mut self, format: SampleFormat) -> Result<(), HostAudioError>;

    /// Queue interleaved PCM (`format.bytes_per_frame` per frame).
    fn push_pcm(&mut self, data: &[u8]) -> Result<(), HostAudioError>;

    /// Stop playback and release the device.
    fn close(&mut self);
}

/// Prefer CoreAudio output; fall back to a null sink if no device is available.
#[tracing::instrument(level = "debug", target = "ternvale::virtio::sound", skip_all)]
pub fn open_default_playback() -> Box<dyn PlaybackSink> {
    match CpalPlayback::new() {
        Ok(sink) => {
            tracing::info!(target: "ternvale::virtio::sound", "host audio using CoreAudio (cpal)");
            Box::new(sink)
        }
        Err(err) => {
            tracing::warn!(
                target: "ternvale::virtio::sound",
                error = %err,
                "CoreAudio unavailable; using null playback sink"
            );
            Box::new(NullPlayback::default())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-audio-host");
    }
}
