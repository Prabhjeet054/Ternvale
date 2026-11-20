//! Host microphone capture (CoreAudio through `cpal`). PCM bytes are never logged.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::{HostAudioError, SampleFormat};

enum CaptureCmd {
    Configure(SampleFormat),
    Close,
    Shutdown,
}

/// Pull host PCM into virtio-sound RX. Implementations must not log `data`.
pub trait CaptureSource: Send {
    /// (Re)open the input stream for `format`.
    fn configure(&mut self, format: SampleFormat) -> Result<(), HostAudioError>;

    /// Copy up to `out.len()` captured bytes. Returns bytes written (may be 0).
    fn pull_pcm(&mut self, out: &mut [u8]) -> Result<usize, HostAudioError>;

    /// Stop capture.
    fn close(&mut self);

    /// True while the host input callback is running.
    fn capturing(&self) -> bool;
}

/// Non-zero S16 PCM for `arecord` / Sound Recorder level analogs.
/// Samples are never logged.
#[derive(Debug, Default)]
pub struct ToneCapture {
    format: Option<SampleFormat>,
    live: bool,
}

impl CaptureSource for ToneCapture {
    fn configure(&mut self, format: SampleFormat) -> Result<(), HostAudioError> {
        if format.channels == 0 || format.bytes_per_frame == 0 || format.rate_hz == 0 {
            return Err(HostAudioError::Invalid("zero rate/channels".into()));
        }
        self.format = Some(format);
        self.live = true;
        tracing::info!(
            target: "ternvale::virtio::sound",
            rate_hz = format.rate_hz,
            channels = format.channels,
            "tone capture configured (samples not logged)"
        );
        Ok(())
    }

    fn pull_pcm(&mut self, out: &mut [u8]) -> Result<usize, HostAudioError> {
        if !self.live {
            return Ok(0);
        }
        // ~0.5 amplitude little-endian S16; not silence, not logged.
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = if i % 2 == 0 { 0x00 } else { 0x40 };
        }
        Ok(out.len())
    }

    fn close(&mut self) {
        self.live = false;
        self.format = None;
        tracing::info!(
            target: "ternvale::virtio::sound",
            "tone capture stopped (menu-bar analog)"
        );
    }

    fn capturing(&self) -> bool {
        self.live
    }
}

/// Discard / silence (denied permission, tests, no input device).
#[derive(Debug, Default)]
pub struct NullCapture {
    format: Option<SampleFormat>,
}

impl CaptureSource for NullCapture {
    fn configure(&mut self, format: SampleFormat) -> Result<(), HostAudioError> {
        if format.channels == 0 || format.bytes_per_frame == 0 || format.rate_hz == 0 {
            return Err(HostAudioError::Invalid("zero rate/channels".into()));
        }
        self.format = Some(format);
        Ok(())
    }

    fn pull_pcm(&mut self, out: &mut [u8]) -> Result<usize, HostAudioError> {
        out.fill(0);
        Ok(out.len())
    }

    fn close(&mut self) {
        self.format = None;
    }

    fn capturing(&self) -> bool {
        false
    }
}

/// CoreAudio input on a dedicated thread (`Stream` is not `Send`).
pub struct CpalCapture {
    tx: Sender<CaptureCmd>,
    shared: Arc<Mutex<CaptureBuffer>>,
    live: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl CpalCapture {
    /// Spawn the input thread.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::sound", skip_all)]
    pub fn new() -> Result<Self, HostAudioError> {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(CaptureBuffer::default()));
        let live = Arc::new(AtomicBool::new(false));
        let buf = Arc::clone(&shared);
        let flag = Arc::clone(&live);
        let join = thread::Builder::new()
            .name("ternvale-coreaudio-in".into())
            .spawn(move || capture_thread(rx, buf, flag))
            .map_err(|source| HostAudioError::Unavailable {
                message: source.to_string(),
            })?;
        Ok(Self {
            tx,
            shared,
            live,
            join: Some(join),
        })
    }
}

impl Drop for CpalCapture {
    fn drop(&mut self) {
        let _ = self.tx.send(CaptureCmd::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl CaptureSource for CpalCapture {
    fn configure(&mut self, format: SampleFormat) -> Result<(), HostAudioError> {
        self.tx
            .send(CaptureCmd::Configure(format))
            .map_err(|_| HostAudioError::Unavailable {
                message: "coreaudio capture thread stopped".into(),
            })
    }

    fn pull_pcm(&mut self, out: &mut [u8]) -> Result<usize, HostAudioError> {
        let mut guard = self
            .shared
            .lock()
            .map_err(|_| HostAudioError::Unavailable {
                message: "capture buffer lock poisoned".into(),
            })?;
        Ok(guard.pop(out))
    }

    fn close(&mut self) {
        let _ = self.tx.send(CaptureCmd::Close);
    }

    fn capturing(&self) -> bool {
        self.live.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct CaptureBuffer {
    bytes: VecDeque<u8>,
}

impl CaptureBuffer {
    fn push(&mut self, data: &[u8]) {
        const MAX: usize = 512 * 1024;
        self.bytes.extend(data);
        while self.bytes.len() > MAX {
            self.bytes.pop_front();
        }
    }

    fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.bytes.len());
        for slot in out.iter_mut().take(n) {
            *slot = self.bytes.pop_front().unwrap_or(0);
        }
        n
    }
}

fn capture_thread(
    rx: Receiver<CaptureCmd>,
    shared: Arc<Mutex<CaptureBuffer>>,
    live: Arc<AtomicBool>,
) {
    let mut inner: Option<cpal::Stream> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            CaptureCmd::Configure(fmt) => {
                inner = match open_input(fmt, Arc::clone(&shared), Arc::clone(&live)) {
                    Ok(stream) => Some(stream),
                    Err(err) => {
                        tracing::warn!(
                            target: "ternvale::virtio::sound",
                            error = %err,
                            "host microphone stream failed"
                        );
                        live.store(false, Ordering::Relaxed);
                        None
                    }
                };
            }
            CaptureCmd::Close | CaptureCmd::Shutdown => {
                inner.take();
                live.store(false, Ordering::Relaxed);
                if matches!(cmd, CaptureCmd::Shutdown) {
                    break;
                }
            }
        }
    }
}

fn open_input(
    format: SampleFormat,
    shared: Arc<Mutex<CaptureBuffer>>,
    live: Arc<AtomicBool>,
) -> Result<cpal::Stream, HostAudioError> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| HostAudioError::Unavailable {
            message: "no default input device (microphone permission?)".into(),
        })?;
    tracing::info!(
        target: "ternvale::virtio::sound",
        name = %device.name().unwrap_or_else(|_| "unknown".into()),
        "cpal input device selected"
    );
    let mut config = device
        .default_input_config()
        .map_err(|source| HostAudioError::Unavailable {
            message: source.to_string(),
        })?
        .config();
    config.channels = format.channels as u16;
    config.sample_rate = cpal::SampleRate(format.rate_hz);
    let bpf = format.bytes_per_frame.max(1);
    let stream = device
        .build_input_stream(
            &config,
            {
                let shared = Arc::clone(&shared);
                let live = Arc::clone(&live);
                move |data: &[f32], _| {
                    live.store(true, Ordering::Relaxed);
                    let mut packed = Vec::with_capacity(data.len().saturating_mul(2));
                    // Guest typically wants S16; pack without logging samples.
                    if bpf >= 4 {
                        for sample in data {
                            packed.extend_from_slice(&sample.to_le_bytes());
                        }
                    } else {
                        for sample in data {
                            let q = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                            packed.extend_from_slice(&q.to_le_bytes());
                        }
                    }
                    if let Ok(mut guard) = shared.lock() {
                        guard.push(&packed);
                    }
                }
            },
            move |err| {
                tracing::warn!(
                    target: "ternvale::virtio::sound",
                    error = %err,
                    "cpal capture stream error"
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
    Ok(stream)
}

/// Prefer CoreAudio input; fall back to silence if the device or TCC fails.
#[tracing::instrument(level = "debug", target = "ternvale::virtio::sound", skip_all)]
pub fn open_default_capture() -> Box<dyn CaptureSource> {
    match CpalCapture::new() {
        Ok(src) => {
            tracing::info!(
                target: "ternvale::virtio::sound",
                "host microphone using CoreAudio (cpal)"
            );
            Box::new(src)
        }
        Err(err) => {
            tracing::warn!(
                target: "ternvale::virtio::sound",
                error = %err,
                "CoreAudio capture unavailable; guest capture gets silence"
            );
            Box::new(NullCapture::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_capture_fills_silence_without_being_live() {
        let mut src = NullCapture::default();
        src.configure(SampleFormat {
            rate_hz: 48_000,
            channels: 1,
            bytes_per_frame: 2,
        })
        .expect("fmt");
        let mut buf = [1u8; 8];
        assert_eq!(src.pull_pcm(&mut buf).expect("pull"), 8);
        assert!(buf.iter().all(|&b| b == 0));
        assert!(!src.capturing());
    }

    #[test]
    fn tone_capture_has_levels_like_arecord_and_sound_recorder() {
        let mut src = ToneCapture::default();
        src.configure(SampleFormat {
            rate_hz: 48_000,
            channels: 1,
            bytes_per_frame: 2,
        })
        .expect("fmt");
        let mut buf = [0u8; 8];
        assert_eq!(src.pull_pcm(&mut buf).expect("pull"), 8);
        assert!(
            buf.iter().any(|&b| b != 0),
            "silence would not move a meter"
        );
        assert!(src.capturing());
        src.close();
        assert!(!src.capturing());
        let shown = format!("{src:?}");
        assert!(!shown.contains("0x40"), "{shown}");
    }
}
