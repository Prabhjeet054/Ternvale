//! virtio-gpu scenario: Linux's virtio-gpu DRM driver with fbdev emulation must report the
//! scanout mode the host configured (sysfs `virtual_size` and `fbset`), and two rows the
//! guest writes through /dev/fb0 must reach the host-side frame unchanged.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{last_lines, Pl011, Step};
use ternvale_gpu::{GpuConfig, GpuDisplay, GpuStats, VirtioGpu};
use ternvale_vmm::{ExitReason, Machine, MachineError};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging_at, log_dir, restore_stdin,
    stdin_pipe, write_result,
};

const COMMAND: Duration = Duration::from_secs(30);
/// Not the 1280x800 default, so the guest can only report it if it came from the device.
const MODE: (u32, u32) = (1024, 768);
const SLOT: u32 = 0;
/// Rows `/bin/gpu-check` fills (scripts/make-gpu-initramfs.sh), with their B,G,R,X pixel.
const ROWS: [(u32, [u8; 4]); 2] = [
    (300, [0x11, 0x22, 0x33, 0xff]),
    (301, [0x44, 0x55, 0x66, 0xff]),
];
const LOG_FILTER: &str = "info,ternvale::gpu=debug";

type Captured = Arc<Mutex<Option<(GpuDisplay, Arc<GpuStats>)>>>;

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
    let guard = init_logging_at("boot-gpu", &log_dir, &filter)?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        width = MODE.0,
        height = MODE.1,
        scenario = "gpu",
        "boot harness logs"
    );

    let serial_log = log_dir.join("guest-serial.log");
    let vm = VmConfig {
        name: "boot-gpu".to_string(),
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
    };
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host = guard.log_path().to_path_buf();
    let feeder =
        std::thread::spawn(move || drive(&serial_for_feed, &mut input, &flag, &host, script()));

    let captured: Captured = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&captured);
    let started = Instant::now();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let config = GpuConfig { modes: vec![MODE] };
        let gpu = VirtioGpu::attach(
            SLOT,
            config,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(SLOT),
        )
        .map_err(|error| MachineError::Attach(format!("virtio-gpu: {error}")))?;
        tracing::info!(target: "ternvale::boot", slot = SLOT, base = %format!("{:#x}", gpu.base), "attached virtio-gpu");
        if let Ok(mut slot) = slot.lock() {
            *slot = Some((gpu.display.clone(), Arc::clone(&gpu.stats)));
        }
        Ok(vec![(gpu.base, gpu.size, gpu.device)])
    });
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(&log_dir, &exit, &script_result, &serial);
    script_result?;
    check_guest_view(&serial)?;
    let (display, stats) = captured
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .ok_or("virtio-gpu was never attached")?;
    check_host_frame(&display, &stats)?;
    match exit {
        Ok(ExitReason::SystemOff) => {
            tracing::info!(
                target: "ternvale::boot",
                total_ms = started.elapsed().as_millis() as u64,
                scenario = "gpu",
                "boot harness passed"
            );
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

/// The guest's own report: fbdev geometry from sysfs and from `fbset`.
fn check_guest_view(serial: &str) -> Result<(), String> {
    let (w, h) = MODE;
    let wanted = [
        "fb0 name=virtio_gpudrmfb".to_string(),
        format!("fb0 virtual_size={w},{h}"),
        "fb0 bits_per_pixel=32".to_string(),
        format!("fb0 stride={}", w * 4),
        format!("mode \"{w}x{h}"),
        format!("geometry {w} {h} {w} {h} 32"),
        "gpu-check wrote rows 300-301 rc=0".to_string(),
        "gpu-check readback row 301: 445566ff445566ff".to_string(),
    ];
    let mut missing = Vec::new();
    for line in &wanted {
        match serial.lines().find(|l| l.contains(line.as_str())) {
            Some(found) => {
                tracing::info!(target: "ternvale::boot", line = %found.trim(), "guest fb0 report")
            }
            None => missing.push(line.clone()),
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("guest fb0 report lacks {missing:?}"))
    }
}

/// The host side: scanout 0 shows a `MODE` frame and the guest's rows, pixel for pixel.
fn check_host_frame(display: &GpuDisplay, stats: &GpuStats) -> Result<(), String> {
    let [commands, errors, flushes, transferred] = stats.snapshot();
    tracing::info!(target: "ternvale::boot", commands, errors, flushes, transferred, "virtio-gpu counters at exit");
    let frame = display.frame(0).ok_or("scanout 0 missing")?;
    tracing::info!(
        target: "ternvale::boot",
        width = frame.width,
        height = frame.height,
        format = ?frame.format,
        sequence = frame.sequence,
        "host frame for scanout 0"
    );
    if (frame.width, frame.height) != MODE {
        return Err(format!(
            "host frame is {}x{}, expected {}x{}",
            frame.width, frame.height, MODE.0, MODE.1
        ));
    }
    if errors != 0 || flushes == 0 {
        return Err(format!("virtio-gpu errors={errors} flushes={flushes}"));
    }
    for (row, want) in ROWS {
        let bad = (0..frame.width)
            .filter(|&x| frame.pixel(x, row) != Some(want))
            .collect::<Vec<_>>();
        if let Some(&x) = bad.first() {
            return Err(format!(
                "row {row}: {} of {} pixels differ from {want:02x?}; first at x={x}: {:02x?}; \
                 the pattern is at {:?}; {} rows hold anything non-zero (last {:?})",
                bad.len(),
                frame.width,
                frame.pixel(x, row),
                find_pattern(&frame.pixels, want),
                (0..frame.height)
                    .filter(|&y| (0..frame.width).any(|x| frame.pixel(x, y) != Some([0; 4])))
                    .count(),
                (0..frame.height)
                    .rev()
                    .find(|&y| (0..frame.width).any(|x| frame.pixel(x, y) != Some([0; 4])))
            ));
        }
        tracing::info!(target: "ternvale::boot", row, pixel = %format!("{want:02x?}"), "host frame row matches the guest's write");
    }
    let (first, last) = (ROWS[0].0 - 1, ROWS[1].0 + 1);
    for row in [first, last] {
        if ROWS
            .iter()
            .any(|(_, want)| frame.pixel(0, row) == Some(*want))
        {
            return Err(format!(
                "row {row} also holds the pattern: stride or offset is wrong"
            ));
        }
    }
    Ok(())
}

/// Byte offsets of the first and last 4-aligned occurrence of `pixel` in `pixels`.
fn find_pattern(pixels: &[u8], pixel: [u8; 4]) -> Option<(usize, usize)> {
    let mut hits = pixels
        .chunks_exact(4)
        .enumerate()
        .filter(|(_, p)| *p == pixel)
        .map(|(i, _)| i * 4);
    let first = hits.next()?;
    Some((first, hits.next_back().unwrap_or(first)))
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
        send(b"gpu-check\n"),
        expect("gpu-check done", COMMAND),
        expect("# ", COMMAND),
        send(b"/bin/busybox "),
        send(b"poweroff -f\n"),
    ]
}
