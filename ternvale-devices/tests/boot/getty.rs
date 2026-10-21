//! getty scenario: a busybox getty on the framebuffer console (tty1) must take a login
//! name and then a shell command typed on the virtio-input keyboard. Keys go in as macOS
//! virtual key codes through `mac_to_linux_key`, as the dev viewer's do; the guest's
//! login program and the typed command report on the serial console.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{
    last_lines, mac_to_linux_key, InputKind, InputSink, Pl011, Step, VirtioInput,
};
use ternvale_gpu::{GpuConfig, VirtioGpu};
use ternvale_vmm::{ExitReason, Machine, MachineError};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging_at, log_dir, restore_stdin,
    stdin_pipe, write_result,
};

const COMMAND: Duration = Duration::from_secs(30);
const LOG_FILTER: &str = "info,ternvale::virtio::input=debug";
const CMDLINE: &str =
    "console=tty0 console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init ternvale.getty";
const LOGIN: &str = "ternvale";
const TYPED: &str = "echo \"Typed: Hello, World_$((6*7))!\" >/dev/ttyAMA0\n";
const RESULT: &str = "Typed: Hello, World_42!";
/// macOS `kVK_Shift`.
const KVK_SHIFT: u16 = 0x38;
/// Between key reports, so a whole line never outruns the guest's eventq buffers.
const KEY_GAP: Duration = Duration::from_millis(8);

type Keyboard = Arc<Mutex<Option<InputSink>>>;

/// `(kVK, shift)` for a character on a US keyboard (Carbon `HIToolbox/Events.h`).
fn mac_key(ch: char) -> Option<(u16, bool)> {
    const PLAIN: &str = "asdfhgzxcv\0bqweryt123465=97-80]ou[ip\nlj'k;\\,/nm.\t `";
    const SHIFTED: &str = "ASDFHGZXCV\0BQWERYT!@#$^%+(&_*)}OU{IP\0LJ\"K:|<?NM>\0\0~";
    if ch == '\0' {
        return None;
    }
    let find = |set: &str| set.chars().position(|c| c == ch).map(|i| i as u16);
    find(PLAIN)
        .map(|kvk| (kvk, false))
        .or_else(|| find(SHIFTED).map(|kvk| (kvk, true)))
}

pub fn run() -> Result<(), String> {
    let root = assets_root().join("virtio-gpu");
    for name in ["Image", "initramfs.cpio"] {
        let path = root.join(name);
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-gpu-initramfs.sh)",
                path.display()
            ));
        }
    }
    let log_dir = log_dir();
    let filter = std::env::var("TERNVALE_BOOT_LOG").unwrap_or_else(|_| LOG_FILTER.to_string());
    let guard = init_logging_at("boot-getty", &log_dir, &filter)?;
    tracing::info!(target: "ternvale::boot", dir = %log_dir.display(),
        host_log = %guard.log_path().display(), scenario = "getty", "boot harness logs");

    let serial_log = log_dir.join("guest-serial.log");
    let vm = VmConfig {
        name: "boot-getty".to_string(),
        profile: Default::default(),
        iso: None,
        cpus: boot_cpus(1)?,
        ram_mib: 256,
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
    };
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host = guard.log_path().to_path_buf();
    let feeder =
        std::thread::spawn(move || drive(&serial_for_feed, &mut input, &flag, &host, script()));

    let keyboard: Keyboard = Arc::new(Mutex::new(None));
    let typist = {
        let (keyboard, cancel, serial) = (
            Arc::clone(&keyboard),
            Arc::clone(&cancel),
            serial_log.clone(),
        );
        std::thread::spawn(move || type_session(&serial, &keyboard, &cancel))
    };
    let slot = Arc::clone(&keyboard);
    let started = Instant::now();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let gpu = VirtioGpu::attach(
            0,
            GpuConfig::default(),
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(0),
        )
        .map_err(|error| MachineError::Attach(format!("virtio-gpu: {error}")))?;
        let kbd = VirtioInput::attach(
            1,
            InputKind::Keyboard,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(1),
        )
        .map_err(|error| MachineError::Attach(format!("virtio-input: {error}")))?;
        tracing::info!(target: "ternvale::boot", gpu = %format!("{:#x}", gpu.base),
            keyboard = %format!("{:#x}", kbd.base), "attached virtio-gpu and virtio-input keyboard");
        if let Ok(mut slot) = slot.lock() {
            *slot = Some(kbd.sink);
        }
        Ok(vec![
            (gpu.base, gpu.size, gpu.device),
            (kbd.base, kbd.size, kbd.device),
        ])
    });
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    let typed = typist
        .join()
        .unwrap_or_else(|_| Err("typist panicked".to_string()));
    restore_stdin(saved);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(&log_dir, &exit, &script_result, &serial);
    script_result?;
    typed?;
    for line in serial
        .lines()
        .filter(|l| l.contains("getty login name=") || l.contains(RESULT))
    {
        tracing::info!(target: "ternvale::boot", line = %line.trim(), "guest report from the tty1 session");
    }
    let stats = keyboard
        .lock()
        .ok()
        .and_then(|k| k.as_ref().map(|k| k.stats().snapshot()));
    tracing::info!(target: "ternvale::boot", keyboard = ?stats, "virtio-input counters (injected, dropped, status)");
    if stats.is_none_or(|[_, dropped, _]| dropped != 0) {
        return Err(format!("keyboard dropped events: {stats:?}"));
    }
    match exit {
        Ok(ExitReason::SystemOff) => {
            tracing::info!(target: "ternvale::boot", total_ms = started.elapsed().as_millis() as u64,
                scenario = "getty", "boot harness passed");
            drop(guard);
            Ok(())
        }
        other => Err(format!(
            "exit={other:?}\nlog={}\n--- serial ---\n{}",
            guard.log_path().display(),
            last_lines(&serial, 40)
        )),
    }
}

/// Wait for `pattern` on the serial log.
fn wait_serial(serial: &Path, pattern: &str, cancel: &AtomicBool) -> Result<(), String> {
    let deadline = Instant::now() + banner_timeout() + COMMAND;
    loop {
        if std::fs::read_to_string(serial)
            .unwrap_or_default()
            .contains(pattern)
        {
            return Ok(());
        }
        if cancel.load(Ordering::Acquire) || Instant::now() > deadline {
            return Err(format!("guest never printed {pattern:?}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Type the login name once getty is up, then the command once the shell is.
fn type_session(serial: &Path, keyboard: &Keyboard, cancel: &AtomicBool) -> Result<(), String> {
    wait_serial(serial, "ternvale getty on tty1", cancel)?;
    // getty flushes tty input when it opens tty1; give it time to print its prompt.
    std::thread::sleep(Duration::from_secs(2));
    let sink = keyboard
        .lock()
        .ok()
        .and_then(|k| k.clone())
        .ok_or("keyboard never attached")?;
    type_text(&sink, &format!("{LOGIN}\n"))?;
    wait_serial(
        serial,
        &format!("ternvale getty login name={LOGIN}"),
        cancel,
    )?;
    std::thread::sleep(Duration::from_millis(500));
    type_text(&sink, TYPED)
}

pub fn type_text(sink: &InputSink, text: &str) -> Result<(), String> {
    let shift = mac_to_linux_key(KVK_SHIFT).ok_or("no Linux key for kVK_Shift")?;
    for ch in text.chars() {
        let (kvk, shifted) = mac_key(ch).ok_or_else(|| format!("no macOS key for {ch:?}"))?;
        let code = mac_to_linux_key(kvk).ok_or_else(|| format!("no Linux key for kVK {kvk:#x}"))?;
        let mut presses = vec![(code, true), (code, false)];
        if shifted {
            presses.insert(0, (shift, true));
            presses.push((shift, false));
        }
        for (code, pressed) in presses {
            if !sink.key(code, pressed) {
                return Err(format!("key report for {ch:?} dropped"));
            }
            std::thread::sleep(KEY_GAP);
        }
    }
    tracing::info!(target: "ternvale::boot", chars = text.len(), text = %text.trim_end(), "typed on the virtio-input keyboard");
    Ok(())
}

fn expect(pattern: &str, timeout: Duration) -> Step {
    Step::Expect {
        pattern: pattern.to_string(),
        timeout,
    }
}

// PL011 RX FIFO is 16 bytes; every Send below stays within that.
fn script() -> Vec<Step> {
    let banner = banner_timeout();
    vec![
        expect("Linux version", banner),
        expect("ternvale getty on tty1", banner),
        expect(&format!("ternvale getty login name={LOGIN}"), COMMAND),
        expect(RESULT, COMMAND),
        Step::Send {
            data: b"/bin/busybox ".to_vec(),
        },
        Step::Send {
            data: b"poweroff -f\n".to_vec(),
        },
    ]
}
