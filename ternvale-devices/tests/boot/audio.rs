//! virtio-sound scenario: guest loads virtio_snd, plays a short sine via speaker-test.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{Pl011, Step, VirtioSound};
use ternvale_vmm::{ExitReason, Machine, MachineError};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging_at, log_dir, restore_stdin,
    stdin_pipe, write_result,
};

const LOG_FILTER: &str = "info,ternvale::virtio::sound=debug";
const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init";
const MAX_UNDERRUN_EPISODES: u64 = 5;

pub fn run() -> Result<(), String> {
    let root = assets_root().join("virtio-audio");
    for name in ["Image", "initramfs.cpio"] {
        let path = root.join(name);
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-audio-initramfs.sh)",
                path.display()
            ));
        }
    }
    let log_dir = log_dir();
    let filter = std::env::var("TERNVALE_BOOT_LOG").unwrap_or_else(|_| LOG_FILTER.to_string());
    let guard = init_logging_at("boot-audio", &log_dir, &filter)?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        scenario = "audio",
        "boot harness logs"
    );

    let serial_log = log_dir.join("guest-serial.log");
    let vm = VmConfig {
        name: "boot-audio".to_string(),
        profile: Default::default(),
        iso: None,
        cpus: boot_cpus(1)?,
        ram_mib: 512,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: CMDLINE.to_string(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: None,
        nvram: None,
        firmware_tables: None,
        vsock: None,
        tpm: None,
        usb: None,
        audio: None,
        shares: Vec::new(),
    };
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host_log = guard.log_path().to_path_buf();
    let feeder = std::thread::spawn(move || {
        drive(
            &serial_for_feed,
            &mut input,
            &flag,
            &host_log,
            vec![
                Step::Expect {
                    pattern: "Linux version".to_string(),
                    timeout: banner_timeout(),
                },
                Step::Expect {
                    pattern: "ternvale audio ready".to_string(),
                    timeout: banner_timeout(),
                },
                Step::Expect {
                    pattern: "audio-check done".to_string(),
                    timeout: Duration::from_secs(90),
                },
            ],
        )
    });

    let started = Instant::now();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let (base, size, device) =
            VirtioSound::attach(0, Arc::clone(&attach.memory), attach.virtio_irq_hook(0))
                .map_err(|error| MachineError::Attach(format!("virtio-sound: {error}")))?;
        tracing::info!(
            target: "ternvale::boot",
            slot = 0u32,
            base = %format!("{base:#x}"),
            "attached virtio-sound for audio boot scenario"
        );
        Ok(vec![(base, size, device)])
    });
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    let host = guard.log_path().to_path_buf();
    let host_text = std::fs::read_to_string(&host).unwrap_or_default();
    drop(guard);
    write_result(&log_dir, &exit, &script, &serial);
    cancel.store(true, Ordering::Release);
    script?;
    verify_host_log(&host_text)?;
    match exit {
        Ok(ExitReason::SystemOff) => {
            tracing::info!(
                target: "ternvale::boot",
                boot_ms = started.elapsed().as_millis() as u64,
                scenario = "audio",
                "boot harness passed"
            );
            Ok(())
        }
        other => Err(format!(
            "exit={other:?}\nlog={}\n--- serial ---\n{}",
            host.display(),
            ternvale_devices::last_lines(&serial, 40)
        )),
    }
}

fn verify_host_log(host: &str) -> Result<(), String> {
    let max = std::env::var("TERNVALE_AUDIO_MAX_UNDERRUNS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(MAX_UNDERRUN_EPISODES);
    if !host.contains("virtio-sound PCM format negotiated") {
        return Err("host log missing virtio-sound PCM format negotiated (INFO)".to_string());
    }
    let warn_lines = host
        .lines()
        .filter(|line| line.contains("host audio buffer underrun"))
        .count();
    let stats_episodes = host
        .lines()
        .filter(|line| line.contains("host audio playback buffer stats"))
        .filter_map(|line| {
            line.split("underrun_episodes=")
                .nth(1)?
                .split_whitespace()
                .next()
        })
        .filter_map(|token| token.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    let episodes = stats_episodes.max(u64::try_from(warn_lines).unwrap_or(u64::MAX));
    if episodes > max {
        return Err(format!(
            "host audio underrun_episodes={episodes} exceeds limit {max}"
        ));
    }
    tracing::info!(
        target: "ternvale::boot",
        underrun_episodes = episodes,
        underrun_warn_lines = warn_lines,
        max_underruns = max,
        "audio host log checks passed"
    );
    Ok(())
}
