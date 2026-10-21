//! virtio-sound protocol helpers.

use super::protocol::{
    encode_pcm_event, encode_pcm_info, parse_set_params, D_OUTPUT, EVT_PCM_PERIOD_ELAPSED, FMT_S16,
    RATE_48000, R_PCM_SET_PARAMS,
};

#[test]
fn pcm_info_is_output_stream() {
    let info = encode_pcm_info();
    assert_eq!(info[24], D_OUTPUT);
}

#[test]
fn set_params_round_trip() {
    let mut bytes = [0u8; 24];
    bytes[0..4].copy_from_slice(&R_PCM_SET_PARAMS.to_le_bytes());
    bytes[8..12].copy_from_slice(&4096u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&1024u32.to_le_bytes());
    bytes[20] = 2;
    bytes[21] = FMT_S16;
    bytes[22] = RATE_48000;
    let params = parse_set_params(&bytes).expect("parse");
    assert_eq!(params.channels, 2);
    assert_eq!(params.rate_hz(), 48_000);
    assert!(params.describe().contains("48000"));
}

#[test]
fn pcm_event_encodes_type_and_stream() {
    let event = encode_pcm_event(EVT_PCM_PERIOD_ELAPSED, 0);
    assert_eq!(event[0..4], EVT_PCM_PERIOD_ELAPSED.to_le_bytes());
    assert_eq!(event[4..8], 0u32.to_le_bytes());
}
