//! Direct Linux boot: virtual xHCI visible in `lsusb` (keyboard VID 0627).

use std::sync::Arc;
use std::time::Instant;

use ternvale_config::VmConfig;
use ternvale_devices::{Pl011, Step};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging_at, log_dir, restore_stdin,
    stdin_pipe, write_result,
};

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    let serial_log = log_dir.join("guest-serial.log");
    let guard = init_logging_at("boot-usb", &log_dir, "info,ternvale::usb=debug")?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        scenario = "usb",
        "boot harness logs"
    );
    let root = assets_root().join("usb");
    for name in ["Image", "initramfs.cpio"] {
        let path = root.join(name);
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-usb-initramfs.sh)",
                path.display()
            ));
        }
    }
    let started = Instant::now();
    let cpus = boot_cpus(1)?;
    let vm = VmConfig {
        name: "boot-usb".to_string(),
        profile: Default::default(),
        iso: None,
        cpus,
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
            vec![
                Step::Expect {
                    pattern: "Linux version".to_string(),
                    timeout: banner_timeout(),
                },
                Step::Expect {
                    pattern: "ternvale usb ready xhci=yes keyboard=0627".to_string(),
                    timeout: banner_timeout(),
                },
            ],
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
                scenario = "usb",
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
