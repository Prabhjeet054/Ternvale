//! Encrypted vs plaintext marker on a layered disk (hypervisor).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::{Disk, VirtioTransport, VmConfig};
use ternvale_crypto::{CryptoContext, Secret};
use ternvale_devices::{attach_blocks_crypto, last_lines, Pl011, Step};
use ternvale_snapshot::{DeltaLayer, ImageStack, StackManifest};
use ternvale_vmm::{ExitReason, Machine, VmControl};

use crate::common::{
    assets_root, banner_timeout, drive, init_logging, log_dir, restore_stdin, stdin_pipe,
    write_result,
};

const DISK_BYTES: u64 = 8 * 1024 * 1024;
const BLOCK_SIZE: u64 = 65_536;
const BLOCK: u64 = 4;
const MARKER: &[u8] = b"TERNVALE-PLAINTEXT-MARKER";
const NEEDLE: &str = "TERNVALE-PLAINTEXT-MARKER";
const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init ternvale.crypto=1";

fn grep_c(path: &Path, needle: &str) -> u32 {
    let out = Command::new("grep")
        .args(["-a", "-c", "--", needle])
        .arg(path)
        .output()
        .unwrap_or_else(|e| panic!("grep {e}"));
    if !out.status.success() {
        return 0;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

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
    let guard = init_logging("boot-encryption", &log_dir)?;
    tracing::info!(
        target: "ternvale::crypto",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        scenario = "encryption",
        "boot harness logs"
    );

    let started = Instant::now();
    let plain = one_vm(&root, &log_dir, guard.log_path(), false)?;
    if grep_c(&plain, NEEDLE) == 0 {
        return Err(format!(
            "unencrypted control delta {} has no marker",
            plain.display()
        ));
    }

    let enc = one_vm(&root, &log_dir, guard.log_path(), true)?;
    if grep_c(&enc, NEEDLE) != 0 {
        return Err(format!(
            "encrypted delta {} still contains plaintext marker",
            enc.display()
        ));
    }

    let log_text = std::fs::read_to_string(guard.log_path()).unwrap_or_default();
    let dek_hex = "5a".repeat(32);
    if log_text.to_ascii_lowercase().contains(&dek_hex) {
        return Err("DEK hex leaked into the host log".into());
    }

    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "encryption",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn one_vm(
    root: &Path,
    log_dir: &Path,
    host_log: &Path,
    encrypted: bool,
) -> Result<PathBuf, String> {
    let tag = if encrypted { "enc" } else { "plain" };
    let stack_dir = log_dir.join(format!("stack-{tag}"));
    std::fs::create_dir_all(&stack_dir).map_err(|e| format!("stack dir: {e}"))?;
    let base = stack_dir.join("base.img");
    seed_zeros(&base, DISK_BYTES)?;
    let delta = stack_dir.join("layer.tdelta");
    let stack_path = stack_dir.join("disk.tstack");
    DeltaLayer::create(&delta, DISK_BYTES).map_err(|e| e.to_string())?;
    StackManifest {
        base: PathBuf::from("base.img"),
        deltas: vec![PathBuf::from("layer.tdelta")],
    }
    .save(&stack_path)
    .map_err(|e| e.to_string())?;

    let serial_log = log_dir.join(format!("{tag}-serial.log"));
    let vm = VmConfig {
        name: format!("boot-encryption-{tag}"),
        profile: Default::default(),
        iso: None,
        cpus: 1,
        ram_mib: 256,
        ram_min_mib: None,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: CMDLINE.to_string(),
        boot_disk: false,
        disks: vec![Disk {
            path: stack_path.clone(),
            read_only: false,
            io_workers: 1,
            ..Default::default()
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
        power: Default::default(),
        encryption: encrypted,
    };
    let uart = Pl011::open(&serial_log).map_err(|e| e.to_string())?;
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host = host_log.to_path_buf();
    let feeder = std::thread::spawn(move || {
        drive(
            &serial_for_feed,
            &mut input,
            &flag,
            &host,
            vec![
                Step::Expect {
                    pattern: "Linux version".to_string(),
                    timeout: banner_timeout(),
                },
                Step::Expect {
                    pattern: "ternvale crypto marker done".to_string(),
                    timeout: Duration::from_secs(90),
                },
            ],
        )
    });

    let control = Arc::new(VmControl::new(&vm.name, vm.cpus));
    let blocks = vm.block_devices();
    let xts = if encrypted {
        let ctx = Arc::new(
            CryptoContext::from_dek(Secret::new(vec![0x5au8; 32])).map_err(|e| e.to_string())?,
        );
        control.set_crypto(Arc::clone(&ctx));
        Some(ctx.xts().clone())
    } else {
        None
    };
    let exit = if encrypted {
        Machine::run_controlled(&vm, Box::new(uart), &control, move |attach| {
            attach_blocks_crypto(attach, &blocks, VirtioTransport::Mmio, xts).map(|a| a.mmio)
        })
    } else {
        Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
            attach_blocks_crypto(attach, &blocks, VirtioTransport::Mmio, None).map(|a| a.mmio)
        })
    };
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(log_dir, &exit, &script_result, &serial);
    script_result?;
    match exit {
        Ok(ExitReason::SystemOff) | Ok(ExitReason::Canceled) => {}
        other => {
            return Err(format!(
                "{tag} exit={other:?}\nlog={}\n--- serial ---\n{}",
                host_log.display(),
                last_lines(&serial, 40)
            ));
        }
    }

    if encrypted {
        let ctx =
            CryptoContext::from_dek(Secret::new(vec![0x5au8; 32])).map_err(|e| e.to_string())?;
        let stack = ImageStack::open_crypto(&stack_path, true, Some(ctx.xts().clone()))
            .map_err(|e| e.to_string())?;
        let mut buf = [0u8; 32];
        stack
            .read_at(&mut buf, BLOCK * BLOCK_SIZE)
            .map_err(|e| e.to_string())?;
        if !buf.starts_with(MARKER) {
            return Err("encrypted stack decrypt does not yield the marker".into());
        }
    }

    Ok(delta)
}

fn seed_zeros(path: &Path, bytes: u64) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.set_len(bytes).map_err(|e| e.to_string())?;
    Ok(())
}
