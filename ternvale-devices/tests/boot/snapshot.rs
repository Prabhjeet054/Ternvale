//! Layered snapshot boot: write markers in two deltas; inspect raw `.tdelta` files.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::{Disk, VmConfig};
use ternvale_devices::{attach_disks, last_lines, Pl011, Step};
use ternvale_snapshot::{DeltaLayer, ImageStack, StackManifest};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, banner_timeout, drive, init_logging, log_dir, restore_stdin, stdin_pipe,
    write_result,
};

const DISK_BYTES: u64 = 8 * 1024 * 1024;
const BLOCK_SIZE: u64 = 65_536;
const BLOCK_A: u64 = 4;
const BLOCK_B: u64 = 8;
const MARK_A: &[u8] = b"TERNVALE_SNAP_A";
const MARK_B: &[u8] = b"TERNVALE_SNAP_B";

pub fn run() -> Result<(), String> {
    let root = assets_root().join("snapshot-boot");
    for name in ["Image", "initramfs.cpio"] {
        let path = root.join(name);
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-snapshot-initramfs.sh)",
                path.display()
            ));
        }
    }

    let log_dir = log_dir();
    let guard = init_logging("boot-snapshot", &log_dir)?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        scenario = "snapshot",
        "boot harness logs"
    );

    let stack_dir = log_dir.join("stack");
    std::fs::create_dir_all(&stack_dir).map_err(|e| format!("stack dir: {e}"))?;
    let base = stack_dir.join("base.img");
    seed_base(&base, DISK_BYTES)?;
    let delta1 = stack_dir.join("snap1.tdelta");
    let delta2 = stack_dir.join("snap2.tdelta");
    let stack_path = stack_dir.join("disk.tstack");
    DeltaLayer::create(&delta1, DISK_BYTES).map_err(|e| e.to_string())?;
    StackManifest {
        base: PathBuf::from("base.img"),
        deltas: vec![PathBuf::from("snap1.tdelta")],
    }
    .save(&stack_path)
    .map_err(|e| e.to_string())?;

    let started = Instant::now();
    boot_phase(
        &root,
        &stack_path,
        &log_dir,
        "phase1",
        "console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init ternvale.snap=1",
        vec![
            Step::Expect {
                pattern: "Linux version".to_string(),
                timeout: banner_timeout(),
            },
            Step::Expect {
                pattern: "ternvale snapshot phase1 done".to_string(),
                timeout: Duration::from_secs(90),
            },
        ],
        guard.log_path(),
    )?;

    let empty1 = empty_delta_len(DISK_BYTES);
    let size1 = file_len(&delta1);
    tracing::info!(
        target: "ternvale::snapshot",
        bytes = size1,
        empty = empty1,
        "delta1 size after phase1"
    );
    if !delta_contains(&delta1, MARK_A) {
        return Err("snap1.tdelta does not contain phase1 marker".into());
    }
    if delta_contains(&delta1, MARK_B) {
        return Err("snap1.tdelta should not contain phase2 marker yet".into());
    }
    if size1 >= DISK_BYTES / 2 {
        return Err(format!(
            "snap1.tdelta grew to {size1}, expected << {DISK_BYTES}"
        ));
    }

    DeltaLayer::create(&delta2, DISK_BYTES).map_err(|e| e.to_string())?;
    StackManifest {
        base: PathBuf::from("base.img"),
        deltas: vec![PathBuf::from("snap1.tdelta"), PathBuf::from("snap2.tdelta")],
    }
    .save(&stack_path)
    .map_err(|e| e.to_string())?;

    boot_phase(
        &root,
        &stack_path,
        &log_dir,
        "phase2",
        "console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init ternvale.snap=2",
        vec![
            Step::Expect {
                pattern: "Linux version".to_string(),
                timeout: banner_timeout(),
            },
            Step::Expect {
                pattern: "ternvale snapshot phase2 done".to_string(),
                timeout: Duration::from_secs(90),
            },
        ],
        guard.log_path(),
    )?;

    if !delta_contains(&delta1, MARK_A) || delta_contains(&delta1, MARK_B) {
        return Err("snap1.tdelta should still hold only marker A".into());
    }
    if !delta_contains(&delta2, MARK_B) || delta_contains(&delta2, MARK_A) {
        return Err("snap2.tdelta should hold only marker B".into());
    }

    let stack = ImageStack::open(&stack_path, true).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 32];
    stack
        .read_at(&mut buf, BLOCK_A * BLOCK_SIZE)
        .map_err(|e| e.to_string())?;
    if !buf.starts_with(MARK_A) {
        return Err("stack read block A failed".into());
    }
    stack
        .read_at(&mut buf, BLOCK_B * BLOCK_SIZE)
        .map_err(|e| e.to_string())?;
    if !buf.starts_with(MARK_B) {
        return Err("stack read block B failed".into());
    }
    let mut base_byte = [0u8; 1];
    stack
        .read_at(&mut base_byte, BLOCK_SIZE)
        .map_err(|e| e.to_string())?;
    if base_byte[0] != 0xEE {
        return Err("read-through to base block 1 failed".into());
    }

    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "snapshot",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn boot_phase(
    root: &Path,
    stack_path: &Path,
    log_dir: &Path,
    label: &str,
    cmdline: &str,
    steps: Vec<Step>,
    host_log: &Path,
) -> Result<(), String> {
    let serial_log = log_dir.join(format!("{label}-serial.log"));
    let vm = VmConfig {
        name: format!("boot-snapshot-{label}"),
        profile: Default::default(),
        iso: None,
        cpus: 1,
        ram_mib: 256,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: cmdline.to_string(),
        boot_disk: false,
        disks: vec![Disk {
            path: stack_path.to_path_buf(),
            read_only: false,
        }],
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
    let uart = Pl011::open(&serial_log).map_err(|e| e.to_string())?;
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host = host_log.to_path_buf();
    let feeder =
        std::thread::spawn(move || drive(&serial_for_feed, &mut input, &flag, &host, steps));
    let disk = stack_path.to_path_buf();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        attach_disks(attach, &[(disk, false)])
    });
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(log_dir, &exit, &script_result, &serial);
    script_result?;
    match exit {
        Ok(ExitReason::SystemOff) => Ok(()),
        other => Err(format!(
            "{label} exit={other:?}\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 40)
        )),
    }
}

fn seed_base(path: &Path, bytes: u64) -> Result<(), String> {
    use std::os::unix::fs::FileExt;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.set_len(bytes).map_err(|e| e.to_string())?;
    let chunk = [0xEEu8; 512];
    let mut off = 0u64;
    while off < bytes {
        let n = chunk.len().min((bytes - off) as usize);
        file.write_all_at(&chunk[..n], off)
            .map_err(|e| e.to_string())?;
        off += n as u64;
    }
    Ok(())
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn empty_delta_len(virtual_size: u64) -> u64 {
    use ternvale_snapshot::{bitmap_bytes_for_blocks, DeltaHeader, DELTA_HEADER_LEN};
    let header = DeltaHeader {
        virtual_size,
        block_size: ternvale_snapshot::DEFAULT_BLOCK_SIZE,
        bitmap_offset: DELTA_HEADER_LEN as u64,
        bitmap_bytes: 0,
        data_offset: 0,
    };
    let bitmap = bitmap_bytes_for_blocks(header.block_count());
    DELTA_HEADER_LEN as u64 + bitmap
}

fn delta_contains(path: &Path, needle: &[u8]) -> bool {
    std::fs::read(path)
        .map(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
        .unwrap_or(false)
}
