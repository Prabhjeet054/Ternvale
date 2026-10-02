//! Rootfs persistence scenario: boot, write a file, power off, boot again, verify.
//!
//! `rootfs` attaches the disk over virtio-mmio; `pci` attaches the same image
//! as a virtio-pci function (00:01.0) behind the ECAM host bridge, adds
//! `data.ext4` at 00:02.0, and runs the checks in [`crate::pci`].

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::{Disk, VmConfig};
use ternvale_devices::{attach_disks, attach_disks_pci, last_lines, Pl011, Step};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, banner_timeout, drive, init_logging, log_dir, restore_stdin, stdin_pipe,
    write_result,
};
use crate::pci;

/// How the root disk reaches the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Mmio,
    Pci,
}

impl Transport {
    fn scenario(self) -> &'static str {
        match self {
            Self::Mmio => "rootfs",
            Self::Pci => "pci",
        }
    }
}

pub fn run() -> Result<(), String> {
    run_with(Transport::Mmio)
}

pub fn run_pci() -> Result<(), String> {
    run_with(Transport::Pci)
}

fn run_with(transport: Transport) -> Result<(), String> {
    let scenario = transport.scenario();
    let root = assets_root().join("virtio-root");
    for name in ["Image", "initramfs.cpio", "rootfs.ext4"] {
        let path = root.join(name);
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-rootfs.sh)",
                path.display()
            ));
        }
    }
    if transport == Transport::Pci && !root.join("data.ext4").is_file() {
        return Err(format!(
            "missing {} (run ./scripts/make-pci-assets.sh)",
            root.join("data.ext4").display()
        ));
    }

    let log_dir = log_dir();
    let guard = init_logging(&format!("boot-{scenario}"), &log_dir)?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        scenario,
        "boot harness logs"
    );

    // Work on a copy so repeated harness runs do not share dirty state from a crash.
    let image = log_dir.join("rootfs.ext4");
    std::fs::copy(root.join("rootfs.ext4"), &image).map_err(|err| format!("copy rootfs: {err}"))?;
    let mut disks = vec![image.clone()];
    if transport == Transport::Pci {
        let data = log_dir.join("data.ext4");
        std::fs::copy(root.join("data.ext4"), &data)
            .map_err(|err| format!("copy data disk: {err}"))?;
        disks.push(data);
    }

    let started = Instant::now();
    let boot = |label: &str, script: Vec<Step>| {
        run_once(
            &root,
            &disks,
            &log_dir,
            label,
            script,
            guard.log_path(),
            transport,
        )
    };
    boot("boot1", write_script(transport))?;
    tracing::info!(
        target: "ternvale::boot",
        boot1_ms = started.elapsed().as_millis() as u64,
        scenario,
        "rootfs first boot complete"
    );

    let second = Instant::now();
    boot("boot2", verify_script(transport))?;
    tracing::info!(
        target: "ternvale::boot",
        boot2_ms = second.elapsed().as_millis() as u64,
        total_ms = started.elapsed().as_millis() as u64,
        scenario,
        image = %image.display(),
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn run_once(
    root: &Path,
    disks: &[PathBuf],
    log_dir: &Path,
    label: &str,
    script: Vec<Step>,
    host_log: &Path,
    transport: Transport,
) -> Result<(), String> {
    let serial_log = log_dir.join(format!("{label}-serial.log"));
    let vm = VmConfig {
        name: format!("boot-{}-{label}", transport.scenario()),
        cpus: 1,
        ram_mib: 256,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: String::new(),
        boot_disk: true,
        disks: disks
            .iter()
            .map(|path| Disk {
                path: path.clone(),
                read_only: false,
            })
            .collect(),
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: None,
    };
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host = host_log.to_path_buf();
    let feeder =
        std::thread::spawn(move || drive(&serial_for_feed, &mut input, &flag, &host, script));

    let attached: Vec<(PathBuf, bool)> = disks.iter().map(|path| (path.clone(), false)).collect();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let disks = attached.as_slice();
        match transport {
            Transport::Mmio => attach_disks(attach, disks),
            Transport::Pci => attach_disks_pci(attach, disks).map(|_| Vec::new()),
        }
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
        Ok(ExitReason::SystemOff) => {
            tracing::info!(
                target: "ternvale::boot",
                label,
                "rootfs boot reached SystemOff"
            );
            Ok(())
        }
        other => Err(format!(
            "{label} exit={other:?}\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 40)
        )),
    }
}

const COMMAND: Duration = Duration::from_secs(30);

// PL011 RX FIFO is 16 bytes; every Send stays within that.
pub fn send(data: &[u8]) -> Step {
    Step::Send {
        data: data.to_vec(),
    }
}

pub fn expect(pattern: &str, timeout: Duration) -> Step {
    Step::Expect {
        pattern: pattern.to_string(),
        timeout,
    }
}

fn boot_to_shell(transport: Transport) -> Vec<Step> {
    let banner = banner_timeout();
    let mut steps = vec![expect("Linux version", banner)];
    if transport == Transport::Pci {
        steps.extend(pci::kernel_probe(banner));
    }
    steps.extend([
        expect("EXT4-fs (vda): mounted filesystem", banner),
        expect("ternvale root ready", banner),
        expect("# ", banner),
    ]);
    if transport == Transport::Pci {
        steps.extend(pci::shell_checks());
    }
    steps
}

/// Flush, remount `/` read-only so the journal is clean for a host `fsck -n`, then power off.
fn clean_poweroff() -> Vec<Step> {
    vec![
        send(b"sync\n"),
        expect("# ", COMMAND),
        send(b"mount -o "),
        send(b"remount,ro /\n"),
        expect("# ", COMMAND),
        send(b"grep vda "),
        send(b"/proc/mounts\n"),
        expect(" ext4 ro,", COMMAND),
        send(b"/bin/busybox "),
        send(b"poweroff -f\n"),
    ]
}

fn write_script(transport: Transport) -> Vec<Step> {
    let mut steps = boot_to_shell(transport);
    steps.extend([
        // A fresh image must not already hold the marker.
        send(b"cat /root/t\n"),
        expect("No such file", COMMAND),
        send(b"echo persist"),
        send(b" > /root/t\n"),
        expect("# ", COMMAND),
    ]);
    if transport == Transport::Pci {
        steps.extend(pci::data_write());
    }
    steps.extend(clean_poweroff());
    steps
}

fn verify_script(transport: Transport) -> Vec<Step> {
    let mut steps = boot_to_shell(transport);
    steps.extend([send(b"cat /root/t\n"), expect("\npersist", COMMAND)]);
    if transport == Transport::Pci {
        steps.extend(pci::data_verify());
    }
    steps.extend(clean_poweroff());
    steps
}
