//! virtio-input scenario: a keyboard and a tablet on virtio-mmio next to the virtio-gpu.
//! The guest's evdev view of both must match what the devices advertise, and events the
//! host injects through `InputSink` must reach `/dev/input/event*` unchanged
//! (`/bin/input-check`, scripts/make-gpu-initramfs.sh).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{
    last_lines, InputEvent, InputKind, InputSink, Pl011, Step, VirtioInput, ABS_X, ABS_Y, BTN_LEFT,
    REL_WHEEL,
};
use ternvale_gpu::{GpuConfig, VirtioGpu};
use ternvale_vmm::{ExitReason, Machine, MachineError};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging_at, log_dir, restore_stdin,
    stdin_pipe, write_result,
};

const COMMAND: Duration = Duration::from_secs(30);
const LOG_FILTER: &str = "info,ternvale::virtio::input=trace";
const GPU_SLOT: u32 = 0;
const KEYBOARD_SLOT: u32 = 1;
const TABLET_SLOT: u32 = 2;
const KEY_A: u16 = 30;
const KEY_LEFTSHIFT: u16 = 42;
const POINTER: (u32, u32) = (1000, 2000);

type Sinks = Arc<Mutex<Option<(InputSink, InputSink)>>>;

/// What the keyboard must deliver, in order.
fn keyboard_events() -> Vec<InputEvent> {
    let syn = InputEvent::syn();
    vec![
        InputEvent::key(KEY_A, true),
        syn,
        InputEvent::key(KEY_A, false),
        syn,
        InputEvent::key(KEY_LEFTSHIFT, true),
        syn,
        InputEvent::key(KEY_LEFTSHIFT, false),
        syn,
    ]
}

/// What the tablet must deliver, in order.
fn tablet_events() -> Vec<InputEvent> {
    let syn = InputEvent::syn();
    vec![
        InputEvent::abs(ABS_X, POINTER.0),
        InputEvent::abs(ABS_Y, POINTER.1),
        syn,
        InputEvent::key(BTN_LEFT, true),
        syn,
        InputEvent::key(BTN_LEFT, false),
        syn,
        InputEvent::rel(REL_WHEEL, -1),
        syn,
    ]
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
    let guard = init_logging_at("boot-input", &log_dir, &filter)?;
    tracing::info!(target: "ternvale::boot", dir = %log_dir.display(),
        host_log = %guard.log_path().display(), scenario = "input", "boot harness logs");

    let serial_log = log_dir.join("guest-serial.log");
    let vm = VmConfig {
        name: "boot-input".to_string(),
        profile: Default::default(),
        iso: None,
        cpus: boot_cpus(1)?,
        ram_mib: 256,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: String::new(),
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

    let sinks: Sinks = Arc::new(Mutex::new(None));
    let injector = {
        let (sinks, cancel, serial) = (Arc::clone(&sinks), Arc::clone(&cancel), serial_log.clone());
        std::thread::spawn(move || inject_when_ready(&serial, &sinks, &cancel))
    };
    let slot = Arc::clone(&sinks);
    let started = Instant::now();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let gpu = VirtioGpu::attach(
            GPU_SLOT,
            GpuConfig::default(),
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(GPU_SLOT),
        )
        .map_err(|error| MachineError::Attach(format!("virtio-gpu: {error}")))?;
        let mut mapped = vec![(gpu.base, gpu.size, gpu.device)];
        let mut attached = Vec::new();
        for (slot, kind) in [
            (KEYBOARD_SLOT, InputKind::Keyboard),
            (TABLET_SLOT, InputKind::Tablet),
        ] {
            let input = VirtioInput::attach(
                slot,
                kind,
                Arc::clone(&attach.memory),
                attach.virtio_irq_hook(slot),
            )
            .map_err(|error| MachineError::Attach(format!("virtio-input: {error}")))?;
            tracing::info!(target: "ternvale::boot", slot, device = kind.short(),
                base = %format!("{:#x}", input.base), "attached virtio-input");
            mapped.push((input.base, input.size, input.device));
            attached.push(input.sink);
        }
        if let (Some(tablet), Some(keyboard), Ok(mut slot)) =
            (attached.pop(), attached.pop(), slot.lock())
        {
            *slot = Some((keyboard, tablet));
        }
        Ok(mapped)
    });
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    let injected = injector
        .join()
        .unwrap_or_else(|_| Err("injector panicked".to_string()));
    restore_stdin(saved);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(&log_dir, &exit, &script_result, &serial);
    script_result?;
    injected?;
    check_guest_devices(&serial)?;
    check_events(&serial, "keyboard", "tablet", &keyboard_events())?;
    check_events(&serial, "tablet", "done", &tablet_events())?;
    let host_log = std::fs::read_to_string(guard.log_path()).unwrap_or_default();
    check_host_counters(&sinks, &host_log)?;
    match exit {
        Ok(ExitReason::SystemOff) => {
            tracing::info!(target: "ternvale::boot",
                total_ms = started.elapsed().as_millis() as u64, scenario = "input",
                "boot harness passed");
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

/// Wait for the guest's `input-check ready`, then inject both devices' events.
fn inject_when_ready(
    serial: &std::path::Path,
    sinks: &Sinks,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let deadline = Instant::now() + banner_timeout() + COMMAND;
    loop {
        let text = std::fs::read_to_string(serial).unwrap_or_default();
        if text.lines().any(|l| l.trim() == "input-check ready") {
            break;
        }
        if cancel.load(Ordering::Acquire) || Instant::now() > deadline {
            return Err("guest never printed `input-check ready`".to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let (keyboard, tablet) = sinks
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .ok_or("virtio-input devices were never attached")?;
    let sent = [
        keyboard.key(KEY_A, true),
        keyboard.key(KEY_A, false),
        keyboard.key(KEY_LEFTSHIFT, true),
        keyboard.key(KEY_LEFTSHIFT, false),
        tablet.move_to(POINTER.0, POINTER.1),
        tablet.key(BTN_LEFT, true),
        tablet.key(BTN_LEFT, false),
        tablet.scroll(0, -1),
    ];
    tracing::info!(target: "ternvale::boot", accepted = ?sent, "injected input reports");
    if sent.iter().all(|&ok| ok) {
        Ok(())
    } else {
        Err(format!("a report was dropped on injection: {sent:?}"))
    }
}

/// The guest lists both devices with the advertised event types.
fn check_guest_devices(serial: &str) -> Result<(), String> {
    // EV_SYN|EV_KEY|EV_REP = bits 0, 1, 20; EV_SYN|EV_KEY|EV_REL|EV_ABS = bits 0..=3.
    let wanted = [
        ("name=Ternvale Virtio Keyboard", "ev=100003"),
        ("name=Ternvale Virtio Tablet", "ev=f "),
    ];
    for (name, ev) in wanted {
        let line = serial
            .lines()
            .find(|l| l.starts_with("input event") && l.contains(name))
            .ok_or_else(|| format!("guest does not list `{name}`"))?;
        if !line.contains(ev) {
            return Err(format!("`{}` lacks {ev:?}", line.trim()));
        }
        tracing::info!(target: "ternvale::boot", line = %line.trim(), "guest input device");
    }
    let tablet = serial
        .lines()
        .find(|l| l.contains("name=Ternvale Virtio Tablet"))
        .unwrap_or_default();
    for field in ["abs=3", "rel=140"] {
        if !tablet.contains(field) {
            return Err(format!("tablet `{}` lacks {field}", tablet.trim()));
        }
    }
    Ok(())
}

/// Events dumped by `od` between `input-check <from>` and `input-check <to>` must be
/// `want`: struct input_event is 24 bytes, type/code/value at offsets 16, 18, 20.
fn check_events(serial: &str, from: &str, to: &str, want: &[InputEvent]) -> Result<(), String> {
    let (start, end) = (format!("input-check {from}"), format!("input-check {to}"));
    let bytes: Vec<u8> = serial
        .lines()
        .map(str::trim)
        .skip_while(|l| *l != start)
        .skip(1)
        .take_while(|l| *l != end)
        .flat_map(str::split_whitespace)
        .filter_map(|t| u8::from_str_radix(t, 16).ok())
        .collect();
    let got: Vec<InputEvent> = bytes
        .chunks_exact(24)
        .map(|e| InputEvent {
            ev_type: u16::from_le_bytes([e[16], e[17]]),
            code: u16::from_le_bytes([e[18], e[19]]),
            value: u32::from_le_bytes([e[20], e[21], e[22], e[23]]),
        })
        .collect();
    tracing::info!(target: "ternvale::boot", device = from, bytes = bytes.len(), events = ?got, "guest read events");
    if got != want {
        return Err(format!("{from}: guest read {got:?}, expected {want:?}"));
    }
    Ok(())
}

/// Nothing dropped, everything injected, and one TRACE line per injected event.
fn check_host_counters(sinks: &Sinks, host_log: &str) -> Result<(), String> {
    let (keyboard, tablet) = sinks
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .ok_or("virtio-input devices were never attached")?;
    let (kbd, tab) = (keyboard.stats().snapshot(), tablet.stats().snapshot());
    tracing::info!(target: "ternvale::boot", keyboard = ?kbd, tablet = ?tab,
        "virtio-input counters (injected, dropped, status)");
    let want = (keyboard_events().len() as u64, tablet_events().len() as u64);
    if (kbd[0], tab[0]) != want || kbd[1] != 0 || tab[1] != 0 {
        return Err(format!(
            "keyboard {kbd:?}, tablet {tab:?}; wanted {want:?} injected, 0 dropped"
        ));
    }
    let traced = host_log
        .lines()
        .filter(|l| l.contains("TRACE") && l.contains("virtio-input event injected"))
        .count() as u64;
    if traced != want.0 + want.1 {
        return Err(format!(
            "{traced} `virtio-input event injected` TRACE lines, wanted {}",
            want.0 + want.1
        ));
    }
    Ok(())
}

// PL011 RX FIFO is 16 bytes; every Send below stays within that.
fn send(data: &[u8]) -> Step {
    Step::Send {
        data: data.to_vec(),
    }
}

fn expect(pattern: &str, timeout: Duration) -> Step {
    Step::Expect {
        pattern: pattern.to_string(),
        timeout,
    }
}

fn script() -> Vec<Step> {
    let banner = banner_timeout();
    vec![
        expect("Linux version", banner),
        expect("ternvale gpu ready fb0=", banner),
        expect("# ", banner),
        send(b"input-check\n"),
        expect("input-check done", COMMAND),
        expect("# ", COMMAND),
        send(b"/bin/busybox "),
        send(b"poweroff -f\n"),
    ]
}
