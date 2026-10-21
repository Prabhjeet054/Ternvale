//! virtio-sound wire format (Linux `virtio_snd.h`).

/// Virtio device id (`VIRTIO_ID_SOUND`).
pub const VIRTIO_SND_ID: u32 = 25;

pub const VQ_CONTROL: u16 = 0;
pub const VQ_EVENT: u16 = 1;
pub const VQ_TX: u16 = 2;
pub const VQ_RX: u16 = 3;

pub const D_OUTPUT: u8 = 0;

pub const R_PCM_INFO: u32 = 0x0100;
pub const R_PCM_SET_PARAMS: u32 = 0x0101;
pub const R_PCM_PREPARE: u32 = 0x0102;
pub const R_PCM_RELEASE: u32 = 0x0103;
pub const R_PCM_START: u32 = 0x0104;
pub const R_PCM_STOP: u32 = 0x0105;

pub const EVT_PCM_PERIOD_ELAPSED: u32 = 0x1100;
pub const EVT_PCM_XRUN: u32 = 0x1101;

pub const S_OK: u32 = 0x8000;
pub const S_BAD_MSG: u32 = 0x8001;
pub const S_NOT_SUPP: u32 = 0x8002;
pub const S_IO_ERR: u32 = 0x8003;

pub const PCM_F_MSG_POLLING: u32 = 2;

pub const FMT_S16: u8 = 5;
pub const FMT_S32: u8 = 15;
pub const FMT_FLOAT: u8 = 17;

pub const RATE_44100: u8 = 6;
pub const RATE_48000: u8 = 7;

/// Negotiated guest PCM parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmParams {
    pub buffer_bytes: u32,
    pub period_bytes: u32,
    pub channels: u8,
    pub format: u8,
    pub rate: u8,
}

impl PcmParams {
    /// Sample rate in Hz.
    #[must_use]
    pub fn rate_hz(self) -> u32 {
        rate_hz(self.rate)
    }

    /// Bytes per interleaved frame.
    #[must_use]
    pub fn bytes_per_frame(self) -> usize {
        let per_sample = bytes_per_sample(self.format);
        usize::from(self.channels) * per_sample
    }

    /// Human-readable description for INFO logs.
    #[must_use]
    pub fn describe(self) -> String {
        format!(
            "{} Hz, {} ch, format {}, period {} B, buffer {} B",
            self.rate_hz(),
            self.channels,
            format_name(self.format),
            self.period_bytes,
            self.buffer_bytes
        )
    }
}

/// Parse `virtio_snd_pcm_set_params`.
pub fn parse_set_params(bytes: &[u8]) -> Option<PcmParams> {
    if bytes.len() < 20 {
        return None;
    }
    let code = u32::from_le_bytes(bytes[0..4].try_into().expect("code"));
    if code != R_PCM_SET_PARAMS {
        return None;
    }
    Some(PcmParams {
        buffer_bytes: u32::from_le_bytes(bytes[8..12].try_into().expect("buf")),
        period_bytes: u32::from_le_bytes(bytes[12..16].try_into().expect("period")),
        channels: bytes[20],
        format: bytes[21],
        rate: bytes[22],
    })
}

/// Build `virtio_snd_pcm_info` for stream 0 (playback).
pub fn encode_pcm_info() -> [u8; 32] {
    let mut out = [0u8; 32];
    out[4..8].copy_from_slice(&(1u32 << PCM_F_MSG_POLLING).to_le_bytes());
    let formats: u64 = (1u64 << FMT_S16) | (1u64 << FMT_S32) | (1u64 << FMT_FLOAT);
    let rates: u64 = (1u64 << RATE_44100) | (1u64 << RATE_48000);
    out[8..16].copy_from_slice(&formats.to_le_bytes());
    out[16..24].copy_from_slice(&rates.to_le_bytes());
    out[24] = D_OUTPUT;
    out[25] = 1; // channels_min
    out[26] = 2; // channels_max
    out
}

/// Build `virtio_snd_event` (type + stream id).
pub fn encode_pcm_event(event_type: u32, stream_id: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0..4].copy_from_slice(&event_type.to_le_bytes());
    out[4..8].copy_from_slice(&stream_id.to_le_bytes());
    out
}

pub fn write_status(buf: &mut [u8], status: u32) {
    if buf.len() >= 4 {
        buf[0..4].copy_from_slice(&status.to_le_bytes());
    }
}

pub fn rate_hz(rate: u8) -> u32 {
    const TABLE: [u32; 14] = [
        5512, 8000, 11025, 16000, 22050, 32000, 44100, 48000, 64000, 88200, 96000, 176400, 192000,
        384000,
    ];
    TABLE.get(rate as usize).copied().unwrap_or(48_000)
}

pub fn bytes_per_sample(format: u8) -> usize {
    match format {
        FMT_S16 => 2,
        FMT_S32 | FMT_FLOAT => 4,
        _ => 2,
    }
}

fn format_name(format: u8) -> &'static str {
    match format {
        FMT_S16 => "S16_LE",
        FMT_S32 => "S32_LE",
        FMT_FLOAT => "FLOAT_LE",
        _ => "unknown",
    }
}
