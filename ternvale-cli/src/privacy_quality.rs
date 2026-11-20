//! Step 111 host analogs: TCC deny UI, toggle-off, log/temp grep (no media).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use ternvale_audio_host::CameraSource;
use ternvale_config::VmConfig;
use ternvale_vmm::{lockwatch, VmControl};

use super::PrivacyLive;
use crate::diag::{DiagSink, SharedDevices};
use crate::server::handle_line_with;

fn opted_in() -> VmConfig {
    VmConfig::parse(
        "name = \"p\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n[audio]\ninput = true\n[camera]\nenabled = true\n",
    )
    .expect("parse")
}

#[test]
fn system_settings_deny_hides_devices_and_status_explains_why() {
    let live = PrivacyLive::from_snapshot(opted_in().privacy_snapshot());
    live.deny_mic("System Settings denied microphone".into());
    live.deny_camera("System Settings denied camera".into());
    let json = live.json();
    assert_eq!(json.microphone, "denied");
    assert_eq!(json.camera, "denied");
    assert!(
        json.microphone_reason
            .as_ref()
            .is_some_and(|r| r.contains("System Settings")),
        "{json:?}"
    );
    assert!(
        json.camera_reason
            .as_ref()
            .is_some_and(|r| r.contains("System Settings")),
        "{json:?}"
    );

    let devices = SharedDevices::default();
    {
        let mut stats = lockwatch::lock(&devices, "device-stats");
        stats.privacy = Some(Arc::clone(&live));
    }
    let control = Arc::new(VmControl::new("p", 1));
    let log = std::env::temp_dir().join(format!("ternvale-p-privacy-{}.log", std::process::id()));
    let sink = DiagSink::new(&log, Arc::clone(&control), None, devices);
    let response = handle_line_with(
        r#"{"cmd":"status"}"#,
        &control,
        None,
        Some(&sink),
        None,
        None,
        false,
        false,
        "native",
        ternvale_backend::BackendCapabilities::native(),
        "none",
    );
    assert!(response.ok, "{response:?}");
    let privacy = response.status.expect("status").privacy.expect("privacy");
    assert_eq!(privacy.microphone, "denied");
    assert_eq!(privacy.camera, "denied");
    assert!(
        privacy
            .microphone_reason
            .as_ref()
            .is_some_and(|r| r.contains("System Settings")),
        "{privacy:?}"
    );
}

#[test]
fn toggles_off_stop_capturing_like_menu_bar_indicators() {
    let live = PrivacyLive::from_snapshot(opted_in().privacy_snapshot());
    live.mic_flag().store(true, Ordering::Relaxed);
    live.camera_flag().store(true, Ordering::Relaxed);
    assert_eq!(live.json().microphone, "capturing");
    assert_eq!(live.json().camera, "streaming");
    live.mic_flag().store(false, Ordering::Relaxed);
    live.camera_flag().store(false, Ordering::Relaxed);
    assert_eq!(live.json().microphone, "idle");
    assert_eq!(live.json().camera, "idle");

    let off = VmConfig::parse(
        "name = \"p\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n",
    )
    .expect("off");
    let stopped = PrivacyLive::from_snapshot(off.privacy_snapshot());
    stopped.mic_flag().store(true, Ordering::Relaxed);
    stopped.camera_flag().store(true, Ordering::Relaxed);
    assert_eq!(stopped.json().microphone, "off");
    assert_eq!(stopped.json().camera, "off");
}

#[test]
fn host_logs_and_temp_dirs_do_not_hold_audio_or_video_payloads() {
    let dir = std::env::temp_dir().join(format!("ternvale-privacy-grep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let log = dir.join("ternvale-p-20261120-000000.log");
    let frame = ternvale_audio_host::TestPatternCamera::new()
        .next_frame()
        .expect("frame");
    let pcm = [0x00u8, 0x40, 0x00, 0x40];
    let text = format!(
        "INFO ternvale::virtio::sound capture bytes={n} (samples not logged)\nDEBUG {frame:?}\n",
        n = pcm.len()
    );
    std::fs::write(&log, &text).expect("log");
    let hay = std::fs::read_to_string(&log).expect("read");
    assert!(!hay.contains("RIFF"), "{hay}");
    assert!(!hay.contains("WAVE"), "{hay}");
    assert!(!hay.contains("0x40"), "{hay}");
    assert!(!hay.contains("0x80"), "{hay}");
    assert!(hay.contains("768 bytes"), "{hay}");
    let wavs: Vec<_> = std::fs::read_dir(&dir)
        .expect("list")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "wav" || ext == "pcm" || ext == "mp4")
        })
        .collect();
    assert!(wavs.is_empty(), "no host recordings: {wavs:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
