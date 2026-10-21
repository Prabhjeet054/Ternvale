//! desktop scenario: the Xorg + twm + xterm guest from `scripts/make-desktop-rootfs.sh`
//! (docs/DESKTOP.md) on virtio-blk root, virtio-gpu and a virtio-input keyboard and
//! tablet. X must come up on the virtio-gpu at 1280x800; a command typed into the
//! xterm (pointer over it, so twm focuses it) must run; tablet moves to known pixels
//! must put the X pointer on the same pixels (`xtrack` in the guest reports them); and
//! a left click over the xterm must be delivered to it (`xclickwait`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{
    attach_disks, last_lines, InputKind, InputSink, Pl011, Step, VirtioInput, BTN_LEFT,
};
use ternvale_gpu::{GpuConfig, VirtioGpu};
use ternvale_vmm::{ExitReason, Machine, MachineError};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging_at, log_dir, restore_stdin,
    stdin_pipe, write_result,
};
use crate::getty::type_text;

const DESKTOP: Duration = Duration::from_secs(90);
const COMMAND: Duration = Duration::from_secs(30);
const LOG_FILTER: &str = "info,ternvale::virtio::input=debug";
const CMDLINE: &str = "console=tty0 console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init";
const SCREEN: (u32, u32) = (1280, 800);
/// Inside the xterm the guest starts at +100+100 (80x24, 6x13 font).
const OVER_XTERM: (u32, u32) = (400, 250);
const POINTS: [(u32, u32); 5] = [(1, 1), (640, 400), (1279, 799), (320, 600), (1000, 123)];
const TYPED: &str = "echo \"Typed in xterm: $((6*7))\" >/dev/ttyAMA0\n";
const RESULT: &str = "Typed in xterm: 42";
const SETTLE: Duration = Duration::from_millis(300);

type Sinks = Arc<Mutex<Option<(InputSink, InputSink)>>>;

pub fn run() -> Result<(), String> {
    let root = assets_root().join("desktop");
    for name in ["Image", "initramfs.cpio", "rootfs.ext4"] {
        let path = root.join(name);
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-desktop-rootfs.sh)",
                path.display()
            ));
        }
    }
    let log_dir = log_dir();
    let filter = std::env::var("TERNVALE_BOOT_LOG").unwrap_or_else(|_| LOG_FILTER.to_string());
    let guard = init_logging_at("boot-desktop", &log_dir, &filter)?;
    tracing::info!(target: "ternvale::boot", dir = %log_dir.display(),
        host_log = %guard.log_path().display(), scenario = "desktop", "boot harness logs");
    let disk = log_dir.join("rootfs.ext4");
    std::fs::copy(root.join("rootfs.ext4"), &disk).map_err(|err| format!("copy rootfs: {err}"))?;

    let serial_log = log_dir.join("guest-serial.log");
    let vm = VmConfig {
        name: "boot-desktop".to_string(),
        profile: Default::default(),
        iso: None,
        cpus: boot_cpus(2)?,
        ram_mib: 1024,
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

    let sinks: Sinks = Arc::new(Mutex::new(None));
    let user = {
        let (sinks, cancel, serial) = (Arc::clone(&sinks), Arc::clone(&cancel), serial_log.clone());
        std::thread::spawn(move || desktop_session(&serial, &sinks, &cancel))
    };
    let slot = Arc::clone(&sinks);
    let started = Instant::now();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let mut devices = attach_disks(attach, &[(disk.as_path(), false)])?;
        let gpu = VirtioGpu::attach(
            1,
            GpuConfig::default(),
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(1),
        )
        .map_err(|error| MachineError::Attach(format!("virtio-gpu: {error}")))?;
        let input = |slot: u32, kind| {
            VirtioInput::attach(
                slot,
                kind,
                Arc::clone(&attach.memory),
                attach.virtio_irq_hook(slot),
            )
            .map_err(|error| MachineError::Attach(format!("virtio-input: {error}")))
        };
        let kbd = input(2, InputKind::Keyboard)?;
        let tab = input(3, InputKind::Tablet)?;
        tracing::info!(target: "ternvale::boot", gpu = %format!("{:#x}", gpu.base),
            keyboard = %format!("{:#x}", kbd.base), tablet = %format!("{:#x}", tab.base),
            "attached virtio-blk root, virtio-gpu, virtio-input keyboard and tablet");
        if let Ok(mut slot) = slot.lock() {
            *slot = Some((kbd.sink, tab.sink));
        }
        devices.extend([
            (gpu.base, gpu.size, gpu.device),
            (kbd.base, kbd.size, kbd.device),
            (tab.base, tab.size, tab.device),
        ]);
        Ok(devices)
    });
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    let session = user
        .join()
        .unwrap_or_else(|_| Err("desktop session panicked".to_string()));
    restore_stdin(saved);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(&log_dir, &exit, &script_result, &serial);
    script_result?;
    session?;
    check_pointer(&serial)?;
    let stats = sinks.lock().ok().and_then(|s| {
        s.as_ref()
            .map(|(k, t)| (k.stats().snapshot(), t.stats().snapshot()))
    });
    tracing::info!(target: "ternvale::boot", counters = ?stats,
        "virtio-input counters (keyboard, tablet) as (injected, dropped, status)");
    if stats.is_none_or(|([_, kd, _], [_, td, _])| kd != 0 || td != 0) {
        return Err(format!("virtio-input dropped events: {stats:?}"));
    }
    match exit {
        Ok(ExitReason::SystemOff) => {
            tracing::info!(target: "ternvale::boot", total_ms = started.elapsed().as_millis() as u64,
                scenario = "desktop", "boot harness passed");
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

/// Absolute value that lands on the centre of guest pixel `(x, y)`, as the viewer computes it.
fn abs_for(x: u32, y: u32) -> (u32, u32) {
    let axis =
        |v: u32, size: u32| ((f64::from(v) + 0.5) / f64::from(size) * 32767.0).round() as u32;
    (axis(x, SCREEN.0), axis(y, SCREEN.1))
}

/// Every `xtrack x=.. y=..` line must sit within 1 px of the matching point.
fn check_pointer(serial: &str) -> Result<(), String> {
    let seen: Vec<(i64, i64)> = serial
        .lines()
        .filter_map(|line| line.trim().strip_prefix("xtrack x="))
        .filter_map(|rest| {
            let (x, y) = rest.split_once(" y=")?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        })
        .collect();
    if seen.len() != POINTS.len() {
        return Err(format!(
            "xtrack saw {} positions, wanted {}: {seen:?}",
            seen.len(),
            POINTS.len()
        ));
    }
    for (&(gx, gy), &(x, y)) in POINTS.iter().zip(&seen) {
        let ok = (x - i64::from(gx)).abs() <= 1 && (y - i64::from(gy)).abs() <= 1;
        tracing::info!(target: "ternvale::boot", host_x = gx, host_y = gy, x_x = x, x_y = y, ok,
            "X pointer position for a tablet move");
        if !ok {
            return Err(format!(
                "tablet move to ({gx},{gy}) put the X pointer at ({x},{y})"
            ));
        }
    }
    Ok(())
}

/// Wait for `pattern` on the serial log; fails at once if the desktop reported failure.
fn wait_serial(serial: &Path, pattern: &str, cancel: &AtomicBool) -> Result<(), String> {
    let deadline = Instant::now() + banner_timeout() + DESKTOP;
    loop {
        let text = std::fs::read_to_string(serial).unwrap_or_default();
        if text.contains(pattern) {
            return Ok(());
        }
        if let Some(line) = text.lines().find(|l| l.contains("ternvale desktop failed")) {
            return Err(format!("guest: {}", line.trim()));
        }
        if cancel.load(Ordering::Acquire) || Instant::now() > deadline {
            return Err(format!("guest never printed {pattern:?}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What a person at the desktop does, through the two virtio-input devices.
fn desktop_session(serial: &Path, sinks: &Sinks, cancel: &AtomicBool) -> Result<(), String> {
    wait_serial(serial, "ternvale desktop ready", cancel)?;
    let (keyboard, tablet) = sinks
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .ok_or("virtio-input devices never attached")?;
    let point = |(x, y): (u32, u32)| -> Result<(), String> {
        let (ax, ay) = abs_for(x, y);
        if !tablet.move_to(ax, ay) {
            return Err(format!("tablet move to ({x},{y}) dropped"));
        }
        std::thread::sleep(SETTLE);
        Ok(())
    };
    point(OVER_XTERM)?;
    type_text(&keyboard, TYPED)?;

    wait_serial(serial, "xtrack ready", cancel)?;
    for p in POINTS {
        point(p)?;
    }
    tracing::info!(target: "ternvale::boot", points = POINTS.len(), "moved the tablet to every point");

    wait_serial(serial, "xclick ready", cancel)?;
    std::thread::sleep(Duration::from_secs(1));
    point(OVER_XTERM)?;
    if !(tablet.key(BTN_LEFT, true) && tablet.key(BTN_LEFT, false)) {
        return Err("left click dropped".to_string());
    }
    tracing::info!(target: "ternvale::boot", x = OVER_XTERM.0, y = OVER_XTERM.1, "clicked over the xterm");
    Ok(())
}

fn expect(pattern: &str, timeout: Duration) -> Step {
    Step::Expect {
        pattern: pattern.to_string(),
        timeout,
    }
}

fn send(data: &[u8]) -> Step {
    Step::Send {
        data: data.to_vec(),
    }
}

// PL011 RX FIFO is 16 bytes; every Send below stays within that.
fn script() -> Vec<Step> {
    let banner = banner_timeout();
    vec![
        expect("Linux version", banner),
        expect("ternvale desktop rootfs ready", banner),
        expect(
            "ternvale desktop ready driver=modesetting",
            banner + DESKTOP,
        ),
        expect(RESULT, COMMAND),
        send(b"xtrack 5\n"),
        expect("xtrack done seen=5", COMMAND),
        send(b"xclickwait\n"),
        expect("xclick window=ternvale-xterm", COMMAND),
        send(b"sync\n"),
        send(b"/bin/busybox "),
        send(b"poweroff -f\n"),
    ]
}
