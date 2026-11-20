//! virtio-sound control parser (PCM_SET_PARAMS). Never interprets PCM bytes.

use crate::quiet;

/// Feed guest-shaped control bytes into the SET_PARAMS parser.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let _ = ternvale_devices::parse_sound_set_params(data);
}
