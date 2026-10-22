//! Full-VM snapshot rewind: capture guest RAM while a serial counter runs, advance
//! the live counter, restore, and confirm ticks resume from the snapshotted value.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{last_lines, Pl011, Step};
use ternvale_vmm::{ExitReason, Machine, VmControl};

use crate::common::{
    assets_root, boot_cpus, drive, init_logging_at, log_dir, restore_stdin, stdin_pipe,
    write_result,
};

const READY: &str = "ternvale vm-snapshot ready";
const LOG_FILTER: &str = "info,ternvale::snapshot=debug,ternvale::vcpu=info";
const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init";
/// Snapshot once the guest has printed this tick (inclusive).
const SNAP_TICK: u32 = 5;
/// Let the live VM run to at least this tick before restore.
const LIVE_TICK: u32 = 12;
/// After restore, post-restore serial must not reach this tick within the observation window.
const LIVE_FLOOR: u32 = 10;

pub fn run() -> Result<(), String> {
    let root = assets_root().join("vm-snapshot");
    for name in ["Image", "initramfs.cpio"] {
        let path = root.join(name);
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-vm-snapshot-initramfs.sh)",
                path.display()
            ));
        }
    }
    let ram_mib = boot_ram_mib();
    let log_dir = log_dir();
    let guard = init_logging_at("boot-vm-snapshot", &log_dir, LOG_FILTER)?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        ram_mib,
        scenario = "vm-snapshot",
        "boot harness logs"
    );

    let serial_log = log_dir.join("guest-serial.log");
    let bundle_dir = log_dir.join("snapshot-bundle");
    let vm = VmConfig {
        name: "boot-vm-snapshot".to_string(),
        profile: Default::default(),
        iso: None,
        cpus: boot_cpus(1)?,
        ram_mib,
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
    let feed_serial = serial_log.clone();
    let host = guard.log_path().to_path_buf();
    let feeder = std::thread::spawn(move || {
        drive(
            &feed_serial,
            &mut input,
            &flag,
            &host,
            vec![Step::Expect {
                pattern: READY.to_string(),
                timeout: Duration::from_secs(120),
            }],
        )
    });

    let control = Arc::new(VmControl::new(&vm.name, vm.cpus));
    let snap_thread = {
        let control = Arc::clone(&control);
        let serial = serial_log.clone();
        let bundle = bundle_dir.clone();
        std::thread::spawn(move || rewind_test(&control, &serial, &bundle, ram_mib))
    };

    let started = Instant::now();
    let exit = Machine::run_controlled(&vm, Box::new(uart), &control, |_| Ok(Vec::new()));
    cancel.store(true, Ordering::Release);
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(&log_dir, &exit, &script, &serial);
    let fail = |why: String| {
        format!(
            "{why}\nexit={exit:?}\nlog={}\n--- serial ---\n{}",
            guard.log_path().display(),
            last_lines(&serial, 50)
        )
    };
    script.map_err(fail)?;
    let summary = match snap_thread.join() {
        Ok(Ok(summary)) => summary,
        Ok(Err(error)) => return Err(fail(error)),
        Err(_) => return Err(fail("snapshot controller panicked".into())),
    };
    if !matches!(exit, Ok(ExitReason::Canceled) | Ok(ExitReason::SystemOff)) {
        return Err(fail(format!("unexpected guest exit: {exit:?}")));
    }
    tracing::info!(
        target: "ternvale::boot",
        scenario = "vm-snapshot",
        ram_mib,
        total_ms = started.elapsed().as_millis() as u64,
        create_ms = summary.create_ms,
        restore_ms = summary.restore_ms,
        snap_tick = summary.tick_at_snap,
        live_tick = summary.tick_before_restore,
        post_restore_max = summary.max_tick_after_restore,
        "vm-snapshot rewind harness passed"
    );
    drop(guard);
    Ok(())
}

struct SnapSummary {
    create_ms: u64,
    restore_ms: u64,
    tick_at_snap: u32,
    tick_before_restore: u32,
    max_tick_after_restore: u32,
}

fn rewind_test(
    control: &Arc<VmControl>,
    serial: &Path,
    bundle: &PathBuf,
    ram_mib: u64,
) -> Result<SnapSummary, String> {
    wait_for(serial, READY, Duration::from_secs(120))?;
    wait_tick_at_least(serial, SNAP_TICK, Duration::from_secs(30))?;
    let tick_at_snap = max_tick(&read_serial(serial)?).unwrap_or(0);
    tracing::info!(
        target: "ternvale::snapshot",
        tick = tick_at_snap,
        ram_mib,
        "creating vm snapshot at counter"
    );
    if bundle.exists() {
        std::fs::remove_dir_all(bundle).map_err(|e| format!("clear bundle: {e}"))?;
    }
    std::fs::create_dir_all(bundle).map_err(|e| format!("mkdir bundle: {e}"))?;
    let create_started = Instant::now();
    control
        .snapshot_create("rewind", bundle)
        .map_err(|e| e.to_string())?;
    let create_ms = millis(create_started.elapsed());
    tracing::info!(
        target: "ternvale::snapshot",
        ram_mib,
        ram_bytes = ram_mib * 1024 * 1024,
        create_ms,
        bundle = %bundle.display(),
        "vm snapshot create timing"
    );

    wait_tick_at_least(serial, LIVE_TICK, Duration::from_secs(30))?;
    let tick_before_restore = max_tick(&read_serial(serial)?).unwrap_or(0);
    if tick_before_restore < LIVE_TICK {
        return Err(format!(
            "live counter only reached {tick_before_restore}, expected >= {LIVE_TICK}"
        ));
    }
    let offset = read_serial(serial)?.len();
    tracing::info!(
        target: "ternvale::snapshot",
        tick = tick_before_restore,
        serial_offset = offset,
        "restoring vm snapshot after live counter advanced"
    );
    let restore_started = Instant::now();
    control
        .snapshot_restore(bundle)
        .map_err(|e| e.to_string())?;
    let restore_ms = millis(restore_started.elapsed());
    tracing::info!(
        target: "ternvale::snapshot",
        ram_mib,
        ram_bytes = ram_mib * 1024 * 1024,
        restore_ms,
        "vm snapshot restore timing"
    );

    std::thread::sleep(Duration::from_secs(5));
    let tail = read_serial(serial)?
        .get(offset..)
        .map(str::to_string)
        .unwrap_or_default();
    let max_after = max_tick(&tail).unwrap_or(0);
    tracing::info!(
        target: "ternvale::snapshot",
        max_tick_after_restore = max_after,
        tail_bytes = tail.len(),
        "post-restore serial ticks"
    );
    if max_after >= LIVE_FLOOR {
        return Err(format!(
            "after restore the counter reached {max_after} in new serial output (expected < {LIVE_FLOOR}, proving rewind; live had reached {tick_before_restore})"
        ));
    }
    if max_after > tick_at_snap + 3 {
        return Err(format!(
            "post-restore max tick {max_after} is too far above snap tick {tick_at_snap}"
        ));
    }
    if max_tick(&tail).is_none() {
        return Err("no TERNVALE_TICK lines after restore in new serial output".into());
    }

    if let Err(error) = control.request_stop(true) {
        tracing::debug!(target: "ternvale::boot", error = %error, "stop after snapshot test");
    }
    Ok(SnapSummary {
        create_ms,
        restore_ms,
        tick_at_snap,
        tick_before_restore,
        max_tick_after_restore: max_after,
    })
}

fn boot_ram_mib() -> u64 {
    match std::env::var("TERNVALE_BOOT_RAM_MIB") {
        Ok(text) => text.parse().unwrap_or_else(|e| {
            tracing::warn!(
                target: "ternvale::boot",
                value = %text,
                error = %e,
                "ignored TERNVALE_BOOT_RAM_MIB"
            );
            256
        }),
        Err(_) => 256,
    }
}

fn read_serial(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))
}

fn wait_for(path: &Path, needle: &str, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if read_serial(path)?.contains(needle) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "timed out waiting for {needle:?} in {}",
        path.display()
    ))
}

fn wait_tick_at_least(path: &Path, tick: u32, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if let Some(max) = max_tick(&read_serial(path)?) {
            if max >= tick {
                tracing::debug!(target: "ternvale::snapshot", max, want = tick, "counter threshold met");
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!(
        "timed out waiting for TERNVALE_TICK >={tick} in {}",
        path.display()
    ))
}

fn max_tick(serial: &str) -> Option<u32> {
    let mut max: Option<u32> = None;
    for line in serial.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("TERNVALE_TICK") {
            if let Ok(num) = rest.trim().parse::<u32>() {
                max = Some(max.map_or(num, |m| m.max(num)));
            }
        }
    }
    max
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    #[test]
    fn max_tick_parses_lines() {
        use super::max_tick;
        let text = "boot\nTERNVALE_TICK 3\nTERNVALE_TICK 10\n";
        assert_eq!(max_tick(text), Some(10));
    }
}
