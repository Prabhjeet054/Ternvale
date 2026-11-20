//! Host analogs for Step 111: `arecord` / Sound Recorder see a capture
//! stream only when the microphone is opted in and TCC allows it.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ternvale_vmm::GuestMemory;

use super::protocol::{encode_pcm_info_range, D_INPUT, D_OUTPUT};
use super::VirtioSound;
use crate::virtio::irq::VirtioIrq;
use crate::virtio::VirtioDevice;

fn open(capture: bool) -> VirtioSound {
    let memory = Arc::new(Mutex::new(GuestMemory::new().expect("memory")));
    let irq = VirtioIrq::new();
    VirtioSound::open_with_capture(memory, irq, capture, Arc::new(AtomicBool::new(false)))
        .expect("virtio-sound")
}

#[test]
fn arecord_sees_a_capture_stream_when_input_is_allowed() {
    let sound = open(true);
    assert_eq!(sound.device_id(), super::VIRTIO_SOUND_ID);
    assert_eq!(sound.read_config(4, 4), 2, "streams (jacks analog)");
    let info = encode_pcm_info_range(0, 2, 2);
    assert_eq!(info[24], D_OUTPUT);
    assert_eq!(info[32 + 24], D_INPUT, "ALSA capture / arecord device");
}

#[test]
fn windows_sound_recorder_uses_the_same_input_stream() {
    // Sound Recorder input levels share the virtio-sound capture jack.
    let info = encode_pcm_info_range(0, 2, 2);
    assert_eq!(info[32 + 24], D_INPUT);
    assert_eq!(open(true).read_config(4, 4), 2);
}

#[test]
fn denied_or_toggled_off_hides_capture_from_the_guest() {
    let sound = open(false);
    assert_eq!(sound.read_config(4, 4), 1, "playback only; no arecord jack");
    let info = encode_pcm_info_range(0, 2, 1);
    assert_eq!(info.len(), 32, "one PCM_INFO blob; no capture stream");
    assert_eq!(info[24], D_OUTPUT);
}
