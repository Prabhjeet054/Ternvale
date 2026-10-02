//! Initrd busybox scenario for the boot harness.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{Pl011, Step};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, banner_timeout, drive, init_logging, log_dir, restore_stdin, stdin_pipe,
    write_result,
};

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    let serial_log = log_dir.join("guest-serial.log");
    let guard = init_logging("boot", &log_dir)?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        scenario = "initrd",
        "boot harness logs"
    );

    let started = Instant::now();
    let cmdline = std::env::var("TERNVALE_BOOT_CMDLINE").unwrap_or_default();
    if !cmdline.is_empty() {
        tracing::info!(target: "ternvale::boot", cmdline, "boot harness cmdline override");
    }
    let root = assets_root();
    let vm = VmConfig {
        name: "boot".to_string(),
        cpus: 1,
        ram_mib: 256,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline,
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: None,
    };
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
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
            initrd_script(),
        )
    });

    let exit = Machine::run_until(&vm, Box::new(uart), Arc::clone(&cancel));
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    let host = guard.log_path().to_path_buf();
    drop(guard);
    write_result(&log_dir, &exit, &script, &serial);
    script?;
    match exit {
        Ok(ExitReason::SystemOff) => {
            tracing::info!(
                target: "ternvale::boot",
                boot_ms = started.elapsed().as_millis() as u64,
                scenario = "initrd",
                "boot harness passed"
            );
            Ok(())
        }
        other => Err(format!(
            "exit={other:?}\nlog={}\n--- serial ---\n{}",
            host.display(),
            ternvale_devices::last_lines(&serial, 20)
        )),
    }
}

fn initrd_script() -> Vec<Step> {
    let banner = banner_timeout();
    let command = Duration::from_secs(15);
    vec![
        Step::Expect {
            pattern: "Linux version".to_string(),
            timeout: banner,
        },
        Step::Expect {
            pattern: "# ".to_string(),
            timeout: banner,
        },
        Step::Send {
            data: b"/bin/busybox ".to_vec(),
        },
        Step::Send {
            data: b"uname -a\n".to_vec(),
        },
        Step::Expect {
            pattern: "aarch64".to_string(),
            timeout: command,
        },
        Step::Send {
            data: b"echo OK\n".to_vec(),
        },
        Step::Expect {
            pattern: "\nOK".to_string(),
            timeout: command,
        },
        Step::Send {
            data: b"/bin/busybox ".to_vec(),
        },
        Step::Send {
            data: b"poweroff -f\n".to_vec(),
        },
    ]
}
