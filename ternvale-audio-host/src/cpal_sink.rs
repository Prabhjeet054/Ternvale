//! CoreAudio output through `cpal` on a dedicated thread (`Stream` is not `Send`).

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::{HostAudioError, PlaybackSink, SampleFormat};

enum AudioCmd {
    Configure(SampleFormat),
    Pcm(Vec<u8>),
    Close,
    Shutdown,
}

/// Handle to the CoreAudio thread (safe to use from the virtio-sound worker).
pub struct CpalPlayback {
    tx: Sender<AudioCmd>,
    join: Option<JoinHandle<()>>,
}

impl CpalPlayback {
    /// Spawn the output thread.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::sound", skip_all)]
    pub fn new() -> Result<Self, HostAudioError> {
        let (tx, rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("ternvale-coreaudio".into())
            .spawn(move || audio_thread(rx))
            .map_err(|source| HostAudioError::Unavailable {
                message: source.to_string(),
            })?;
        Ok(Self {
            tx,
            join: Some(join),
        })
    }
}

impl Drop for CpalPlayback {
    fn drop(&mut self) {
        let _ = self.tx.send(AudioCmd::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl PlaybackSink for CpalPlayback {
    fn configure(&mut self, format: SampleFormat) -> Result<(), HostAudioError> {
        self.tx
            .send(AudioCmd::Configure(format))
            .map_err(|_| HostAudioError::Unavailable {
                message: "coreaudio thread stopped".into(),
            })
    }

    fn push_pcm(&mut self, data: &[u8]) -> Result<(), HostAudioError> {
        self.tx
            .send(AudioCmd::Pcm(data.to_vec()))
            .map_err(|_| HostAudioError::Unavailable {
                message: "coreaudio thread stopped".into(),
            })
    }

    fn close(&mut self) {
        let _ = self.tx.send(AudioCmd::Close);
    }
}

fn audio_thread(rx: Receiver<AudioCmd>) {
    let mut inner: Option<CpalStream> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            AudioCmd::Configure(fmt) => {
                inner = CpalStream::open(fmt).ok();
            }
            AudioCmd::Pcm(data) => {
                if let Some(stream) = inner.as_mut() {
                    stream.push(&data);
                }
            }
            AudioCmd::Close => {
                if let Some(stream) = inner.take() {
                    if let Ok(guard) = stream.shared.lock() {
                        guard.log_stats();
                    }
                }
            }
            AudioCmd::Shutdown => {
                if let Some(stream) = inner.take() {
                    if let Ok(guard) = stream.shared.lock() {
                        guard.log_stats();
                    }
                }
                break;
            }
        }
    }
}

struct CpalStream {
    shared: std::sync::Arc<std::sync::Mutex<AudioBuffer>>,
    _stream: cpal::Stream,
}

impl CpalStream {
    fn open(format: SampleFormat) -> Result<Self, HostAudioError> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        use cpal::BufferSize;

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| HostAudioError::Unavailable {
                message: "no default output device".into(),
            })?;
        tracing::debug!(
            target: "ternvale::virtio::sound",
            name = %device.name().unwrap_or_else(|_| "unknown".into()),
            "cpal output device selected"
        );
        let mut config = device
            .default_output_config()
            .map_err(|source| HostAudioError::Unavailable {
                message: source.to_string(),
            })?
            .config();
        config.channels = format.channels as u16;
        config.sample_rate = cpal::SampleRate(format.rate_hz);
        config.buffer_size = BufferSize::Default;
        let shared = std::sync::Arc::new(std::sync::Mutex::new(AudioBuffer {
            bytes: std::collections::VecDeque::new(),
            bytes_per_frame: format.bytes_per_frame,
            underrun_episodes: 0,
            underrun_warned: false,
        }));
        let channels = config.channels as usize;
        let stream = device
            .build_output_stream(
                &config,
                {
                    let shared = std::sync::Arc::clone(&shared);
                    move |data: &mut [f32], _| {
                        if let Ok(mut guard) = shared.lock() {
                            guard.pop_frames(data, channels);
                        }
                    }
                },
                move |err| {
                    tracing::warn!(
                        target: "ternvale::virtio::sound",
                        error = %err,
                        "cpal stream error"
                    );
                },
                None,
            )
            .map_err(|source| HostAudioError::Unavailable {
                message: source.to_string(),
            })?;
        stream
            .play()
            .map_err(|source| HostAudioError::Unavailable {
                message: source.to_string(),
            })?;
        Ok(Self {
            shared,
            _stream: stream,
        })
    }

    fn push(&mut self, data: &[u8]) {
        if let Ok(mut guard) = self.shared.lock() {
            guard.push(data);
        }
    }
}

struct AudioBuffer {
    bytes: std::collections::VecDeque<u8>,
    bytes_per_frame: usize,
    underrun_episodes: u64,
    underrun_warned: bool,
}

impl AudioBuffer {
    fn push(&mut self, data: &[u8]) {
        self.bytes.extend(data);
        self.underrun_warned = false;
    }

    fn log_stats(&self) {
        tracing::info!(
            target: "ternvale::virtio::sound",
            underrun_episodes = self.underrun_episodes,
            "host audio playback buffer stats"
        );
    }

    fn pop_frames(&mut self, out: &mut [f32], channels: usize) {
        let bpf = self.bytes_per_frame.max(1);
        let frames = out.len() / channels.max(1);
        for frame_index in 0..frames {
            if self.bytes.len() < bpf {
                if !self.underrun_warned {
                    self.underrun_episodes += 1;
                    tracing::warn!(
                        target: "ternvale::virtio::sound",
                        episodes = self.underrun_episodes,
                        "host audio buffer underrun (CoreAudio callback starved)"
                    );
                    self.underrun_warned = true;
                }
                let base = frame_index * channels;
                for sample in &mut out[base..] {
                    *sample = 0.0;
                }
                return;
            }
            let mut frame = [0u8; 8];
            let take = bpf.min(frame.len());
            for byte in frame.iter_mut().take(take) {
                *byte = self.bytes.pop_front().unwrap_or(0);
            }
            let base = frame_index * channels;
            for ch in 0..channels {
                out[base + ch] = f32_from_frame_bytes(&frame, bpf, ch);
            }
        }
    }
}

fn f32_from_frame_bytes(frame: &[u8], bpf: usize, channel: usize) -> f32 {
    if bpf >= 4 * (channel + 1) {
        let off = channel * 4;
        let bits = i32::from_le_bytes(frame[off..off + 4].try_into().expect("i32"));
        return (bits as f32) / i32::MAX as f32;
    }
    if bpf >= 2 * (channel + 1) {
        let off = channel * 2;
        let bits = i16::from_le_bytes(frame[off..off + 2].try_into().expect("i16"));
        return (bits as f32) / i16::MAX as f32;
    }
    0.0
}
