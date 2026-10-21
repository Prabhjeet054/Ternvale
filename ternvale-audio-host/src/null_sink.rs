//! Discard PCM (tests / headless hosts).

use crate::{HostAudioError, PlaybackSink, SampleFormat};

/// No-op playback for CI and hosts without an output device.
#[derive(Debug, Default)]
pub struct NullPlayback {
    format: Option<SampleFormat>,
}

impl PlaybackSink for NullPlayback {
    fn configure(&mut self, format: SampleFormat) -> Result<(), HostAudioError> {
        if format.channels == 0 || format.bytes_per_frame == 0 || format.rate_hz == 0 {
            return Err(HostAudioError::Invalid("zero rate/channels".into()));
        }
        self.format = Some(format);
        Ok(())
    }

    fn push_pcm(&mut self, _data: &[u8]) -> Result<(), HostAudioError> {
        Ok(())
    }

    fn close(&mut self) {
        self.format = None;
    }
}
